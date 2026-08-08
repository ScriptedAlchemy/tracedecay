//! Admitted Work-family synthesis over fan-out sibling evidence (Plan 32).
//!
//! Synthesis is another admitted attempt under the same deadline,
//! cancellation generation, and effect ledger as every other attempt — never
//! a rewrite of the evidence it consumes. Admission seals the ordered source
//! envelopes it was asked to synthesize: each sibling attempt's terminal
//! outcome is captured verbatim from the Work authority (success with its
//! artifact digests, failure with its sealed evidence digest, or a
//! still-unknown state), so failures, unknowns, disagreement, and minority
//! evidence survive into the admission record instead of being collapsed.
//!
//! The admission also fixes the citation obligation: a
//! [`WorkflowSynthesisDraft`] citing every citable source digest, complete by
//! construction, which the workflow completion path verifies through the
//! landed [`crate::workflow_synthesis::verify_workflow_synthesis_draft`].
//! When no source contributed citable evidence there is nothing to
//! synthesize, and the operation returns the sealed, unsynthesized envelope
//! set as a typed outcome rather than admitting an attempt that could only
//! fabricate citations.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    ManifestDigest, WorkAttemptIdentityV1, WorkAttemptStateV1, WorkAttemptV1,
    WorkTerminalEvidenceV1, WorkflowOutputName, canonical_sha256,
};

use crate::work::WorkStoragePort;
use crate::work_attempt::{
    StartWorkAttemptCommand, WorkAttemptService, WorkAttemptStatusRequestV1, WorkAttemptStoragePort,
};
use crate::work_read::WorkProjectionReadPort;
use crate::workflow_synthesis::WorkflowSynthesisDraft;
use crate::{ApplicationProblem, LegalAction, RequestContext, RetryDirective, SafeDiagnostic};

const WORK_SYNTHESIS_SOURCE_SET_DOMAIN: &str =
    "tracedecay.application.work-synthesis-source-set.v1";
const WORK_SYNTHESIS_REQUEST_DOMAIN: &str = "tracedecay.application.work-synthesis-request.v1";

/// One sibling attempt's terminal contribution, captured verbatim from the
/// Work authority at admission time.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum WorkSynthesisSourceOutcomeV1 {
    /// The source succeeded; its declared artifact digests, in declaration
    /// order, are the evidence a synthesis may cite.
    Succeeded { artifacts: Vec<ManifestDigest> },
    /// The source failed; its sealed terminal evidence digest is preserved
    /// so the failure stays visible in the synthesis record.
    Failed { evidence: ManifestDigest },
    /// The source timed out; preserved like a failure.
    TimedOut { evidence: ManifestDigest },
    /// The source was cancelled; preserved like a failure.
    Cancelled { evidence: ManifestDigest },
    /// The source has not reached a terminal state. The unknown is preserved
    /// as an unknown; it contributes no citable evidence and is never
    /// guessed at.
    Unknown { state: WorkAttemptStateV1 },
}

/// One immutable source envelope: which attempt, and what it truthfully
/// contributed at admission time.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkSynthesisSourceEnvelopeV1 {
    pub source: WorkAttemptIdentityV1,
    pub outcome: WorkSynthesisSourceOutcomeV1,
}

/// The ordered, digest-sealed source envelope set a synthesis admission
/// consumed. Reordering or mutating any envelope changes the digest, so a
/// replayed or tampered set is distinguishable from the admitted one.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkSynthesisSourceSetV1 {
    /// The envelopes, exactly in the caller's requested order.
    pub sources: Vec<WorkSynthesisSourceEnvelopeV1>,
    /// Canonical digest over the ordered envelopes.
    pub set_digest: ManifestDigest,
}

impl WorkSynthesisSourceSetV1 {
    /// Seals an ordered envelope list under the source-set domain.
    pub fn seal(sources: Vec<WorkSynthesisSourceEnvelopeV1>) -> Result<Self, ApplicationProblem> {
        let set_digest = canonical_sha256(&(WORK_SYNTHESIS_SOURCE_SET_DOMAIN, &sources))
            .map_err(|_| contract_problem())?;
        Ok(Self {
            sources,
            set_digest,
        })
    }

    /// Whether the carried digest still matches the carried envelopes.
    pub fn verified(&self) -> bool {
        canonical_sha256(&(WORK_SYNTHESIS_SOURCE_SET_DOMAIN, &self.sources))
            .is_ok_and(|digest| digest == self.set_digest)
    }
}

/// Succeeded sources grouped by the exact artifact digest list they produced.
/// Groups are ordered largest first; smaller groups are the minority
/// evidence, and more than one group is the disagreement, preserved as
/// structure instead of being resolved by fiat.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkSynthesisEvidenceGroupV1 {
    /// The artifact digest list every source in this group produced.
    pub artifacts: Vec<ManifestDigest>,
    /// The concurring sources, in the caller's requested order.
    pub sources: Vec<WorkAttemptIdentityV1>,
}

/// Admits one synthesis attempt over an ordered set of sibling sources.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AdmitWorkSynthesisCommand {
    /// The synthesis attempt's own admission facts; it is started through
    /// the same admission machinery as any other attempt.
    pub start: StartWorkAttemptCommand,
    /// The fan-out output the synthesis belongs to, carried into the draft
    /// the workflow completion path verifies.
    pub output_name: WorkflowOutputName,
    /// The ordered sibling sources the synthesis consumes.
    pub sources: Vec<WorkAttemptIdentityV1>,
}

/// Why a synthesis request was answered with the unsynthesized set instead
/// of an admitted attempt.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkSynthesisRefusalV1 {
    /// No source contributed a citable artifact: every source failed, was
    /// cancelled, timed out, is still unknown, or succeeded without
    /// declaring artifacts. There is nothing a synthesis could truthfully
    /// cite.
    NoCitableSources,
}

impl WorkSynthesisRefusalV1 {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::NoCitableSources => "no_citable_sources",
        }
    }
}

/// The admitted synthesis attempt and everything it is accountable to.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkSynthesisAdmissionV1 {
    /// The leased (or idempotently replayed) synthesis attempt.
    pub attempt: WorkAttemptV1,
    /// The sealed source envelopes the attempt was admitted against.
    pub source_set: WorkSynthesisSourceSetV1,
    /// Disagreement structure over the succeeded sources.
    pub groups: Vec<WorkSynthesisEvidenceGroupV1>,
    /// The citation obligation, complete by construction: the draft cites
    /// every citable source digest and is verified downstream by
    /// [`crate::workflow_synthesis::verify_workflow_synthesis_draft`].
    pub draft: WorkflowSynthesisDraft,
    /// Sources preserved without citations — failures, unknowns, and
    /// artifact-less successes — in the caller's requested order.
    pub uncited: Vec<WorkAttemptIdentityV1>,
}

/// The immutable request identity and complete admitted result persisted in
/// the same durable record as the synthesis attempt.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkSynthesisAdmissionRecordV1 {
    pub request_digest: ManifestDigest,
    pub result: WorkSynthesisAdmissionV1,
}

/// The typed outcome of a synthesis request: an admitted attempt, or the
/// sealed unsynthesized set when synthesis could not truthfully begin.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "synthesis")]
pub enum WorkSynthesisAttemptV1 {
    Admitted(Box<WorkSynthesisAdmissionV1>),
    Unsynthesized {
        sources: WorkSynthesisSourceSetV1,
        refusal: WorkSynthesisRefusalV1,
    },
}

/// Validates the source set against the Work authority, seals it, and admits
/// the synthesis attempt through the standard attempt admission. Every
/// source outcome is read from the authority — never trusted from the
/// caller — and preserved verbatim in the admission record.
pub fn admit_work_synthesis<S, P, W>(
    attempts: &WorkAttemptService<S, P, W>,
    context: &RequestContext,
    command: AdmitWorkSynthesisCommand,
) -> Result<WorkSynthesisAttemptV1, ApplicationProblem>
where
    S: WorkAttemptStoragePort,
    P: WorkProjectionReadPort,
    W: WorkStoragePort,
{
    if command.sources.is_empty() {
        return Err(invalid_problem(
            "application.work-synthesis.no-sources",
            "A synthesis attempt must name at least one source attempt.",
        ));
    }
    let mut seen = BTreeSet::new();
    for source in &command.sources {
        if !seen.insert(source.clone()) {
            return Err(invalid_problem(
                "application.work-synthesis.duplicate-source",
                "A synthesis source attempt was named more than once.",
            ));
        }
        if source.task_id() == &command.start.task_id
            && source.run_id() == &command.start.run_id
            && source.attempt_id() == &command.start.attempt_id
        {
            return Err(invalid_problem(
                "application.work-synthesis.self-citation",
                "A synthesis attempt cannot name itself as a source.",
            ));
        }
    }
    let request_digest = canonical_sha256(&(WORK_SYNTHESIS_REQUEST_DOMAIN, &command))
        .map_err(|_| request_identity_problem())?;
    if let Some(replay) = attempts.synthesis_replay(context, &command.start, &request_digest)? {
        return Ok(WorkSynthesisAttemptV1::Admitted(Box::new(replay)));
    }
    let mut envelopes = Vec::with_capacity(command.sources.len());
    for source in &command.sources {
        let attempt = attempts.status(
            context,
            &WorkAttemptStatusRequestV1 {
                task_id: source.task_id().clone(),
                run_id: source.run_id().clone(),
                attempt_id: source.attempt_id().clone(),
            },
        )?;
        envelopes.push(WorkSynthesisSourceEnvelopeV1 {
            source: source.clone(),
            outcome: source_outcome(&attempt)?,
        });
    }
    let cited_source_digests: BTreeSet<ManifestDigest> = envelopes
        .iter()
        .filter_map(|envelope| match &envelope.outcome {
            WorkSynthesisSourceOutcomeV1::Succeeded { artifacts } => Some(artifacts),
            _ => None,
        })
        .flatten()
        .cloned()
        .collect();
    let source_set = WorkSynthesisSourceSetV1::seal(envelopes)?;
    if cited_source_digests.is_empty() {
        return Ok(WorkSynthesisAttemptV1::Unsynthesized {
            sources: source_set,
            refusal: WorkSynthesisRefusalV1::NoCitableSources,
        });
    }
    let groups = evidence_groups(&source_set.sources);
    let uncited = source_set
        .sources
        .iter()
        .filter(|envelope| {
            !matches!(
                &envelope.outcome,
                WorkSynthesisSourceOutcomeV1::Succeeded { artifacts } if !artifacts.is_empty()
            )
        })
        .map(|envelope| envelope.source.clone())
        .collect();
    let admission =
        attempts.start_synthesis(context, command.start, request_digest, move |attempt| {
            WorkSynthesisAdmissionV1 {
                draft: WorkflowSynthesisDraft {
                    output_name: command.output_name,
                    synthesis_attempt: attempt.identity().clone(),
                    cited_source_digests,
                },
                attempt,
                source_set,
                groups,
                uncited,
            }
        })?;
    Ok(WorkSynthesisAttemptV1::Admitted(Box::new(admission)))
}

/// Captures one source attempt's contribution exactly as the authority
/// recorded it.
fn source_outcome(
    attempt: &WorkAttemptV1,
) -> Result<WorkSynthesisSourceOutcomeV1, ApplicationProblem> {
    let evidence_digest = || {
        attempt
            .terminal()
            .map(terminal_digest)
            .ok_or_else(contract_problem)
    };
    Ok(match attempt.state() {
        WorkAttemptStateV1::Succeeded => WorkSynthesisSourceOutcomeV1::Succeeded {
            artifacts: attempt
                .artifacts()
                .iter()
                .map(|artifact| artifact.digest().clone())
                .collect(),
        },
        WorkAttemptStateV1::Failed => WorkSynthesisSourceOutcomeV1::Failed {
            evidence: evidence_digest()?,
        },
        WorkAttemptStateV1::TimedOut => WorkSynthesisSourceOutcomeV1::TimedOut {
            evidence: evidence_digest()?,
        },
        WorkAttemptStateV1::Cancelled => WorkSynthesisSourceOutcomeV1::Cancelled {
            evidence: evidence_digest()?,
        },
        state => WorkSynthesisSourceOutcomeV1::Unknown { state },
    })
}

fn terminal_digest(terminal: &WorkTerminalEvidenceV1) -> ManifestDigest {
    match terminal {
        WorkTerminalEvidenceV1::Succeeded {
            evidence_digest, ..
        }
        | WorkTerminalEvidenceV1::Failed {
            evidence_digest, ..
        }
        | WorkTerminalEvidenceV1::TimedOut {
            evidence_digest, ..
        }
        | WorkTerminalEvidenceV1::Cancelled {
            evidence_digest, ..
        } => evidence_digest.clone(),
    }
}

/// Groups succeeded sources by the exact artifact digest list they produced,
/// largest group first, ties broken by the digest list so the order is
/// deterministic without pretending a tie has a majority.
fn evidence_groups(sources: &[WorkSynthesisSourceEnvelopeV1]) -> Vec<WorkSynthesisEvidenceGroupV1> {
    let mut groups: Vec<WorkSynthesisEvidenceGroupV1> = Vec::new();
    for envelope in sources {
        let WorkSynthesisSourceOutcomeV1::Succeeded { artifacts } = &envelope.outcome else {
            continue;
        };
        if artifacts.is_empty() {
            continue;
        }
        if let Some(group) = groups
            .iter_mut()
            .find(|group| &group.artifacts == artifacts)
        {
            group.sources.push(envelope.source.clone());
        } else {
            groups.push(WorkSynthesisEvidenceGroupV1 {
                artifacts: artifacts.clone(),
                sources: vec![envelope.source.clone()],
            });
        }
    }
    groups.sort_by(|left, right| {
        right
            .sources
            .len()
            .cmp(&left.sources.len())
            .then_with(|| left.artifacts.cmp(&right.artifacts))
    });
    groups
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

fn contract_problem() -> ApplicationProblem {
    ApplicationProblem::unavailable(SafeDiagnostic {
        code: "application.work-synthesis.evidence-inconsistent".to_owned(),
        message: "A terminal source attempt is missing its sealed evidence.".to_owned(),
    })
}

fn request_identity_problem() -> ApplicationProblem {
    ApplicationProblem::unavailable(SafeDiagnostic {
        code: "application.work-synthesis.request-identity-unavailable".to_owned(),
        message: "The synthesis request identity could not be canonicalized.".to_owned(),
    })
}

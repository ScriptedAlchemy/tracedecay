//! Synthesis settlement over fan-out evidence (Plan 32).
//!
//! A synthesis artifact is another admitted output of the same fan-out step,
//! never a rewrite of its evidence: settlement verifies that a claimed
//! synthesis cites every sibling source artifact and that all source evidence
//! remains in the completed output set. A provider that declines synthesis
//! simply returns the unsynthesized evidence set; a provider that claims
//! synthesis without complete citations is a typed protocol violation.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    ManifestDigest, WorkAttemptIdentityV1, WorkflowOutputName, WorkflowStep, WorkflowStepOutput,
};

/// A provider's claim that one artifact of a fan-out output synthesizes its
/// sibling source artifacts.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowSynthesisDraft {
    /// The fan-out output the synthesis belongs to.
    pub output_name: WorkflowOutputName,
    /// The attempt that produced the synthesis artifact inside that output.
    pub synthesis_attempt: WorkAttemptIdentityV1,
    /// Content digests of every source artifact the synthesis consumed.
    pub cited_source_digests: BTreeSet<ManifestDigest>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkflowSynthesisRefusal {
    #[error("synthesis was claimed on a step without fan-out")]
    StepWithoutFanOut,
    #[error("synthesis output is not part of the step result")]
    UnknownOutput,
    #[error("synthesis artifact is not part of the claimed output")]
    UnknownSynthesisArtifact,
    #[error("synthesis has no sibling source evidence to consume")]
    NoSources,
    #[error("synthesis does not cite every source artifact")]
    IncompleteCitations,
    #[error("synthesis cites an artifact outside its source evidence")]
    UnknownCitation,
}

impl WorkflowSynthesisRefusal {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::StepWithoutFanOut => "step_without_fan_out",
            Self::UnknownOutput => "unknown_output",
            Self::UnknownSynthesisArtifact => "unknown_synthesis_artifact",
            Self::NoSources => "no_sources",
            Self::IncompleteCitations => "incomplete_citations",
            Self::UnknownCitation => "unknown_citation",
        }
    }
}

/// Verifies a synthesis claim against the immutable fan-out evidence it must
/// cite. The evidence itself is never modified: acceptance only means the
/// claimed artifact may complete alongside its sources.
pub fn verify_workflow_synthesis_draft(
    step: &WorkflowStep,
    outputs: &[WorkflowStepOutput],
    draft: &WorkflowSynthesisDraft,
) -> Result<(), WorkflowSynthesisRefusal> {
    if step.fan_out.is_none() {
        return Err(WorkflowSynthesisRefusal::StepWithoutFanOut);
    }
    let output = outputs
        .iter()
        .find(|output| output.output_name() == &draft.output_name)
        .ok_or(WorkflowSynthesisRefusal::UnknownOutput)?;
    let mut sources = BTreeSet::new();
    let mut synthesis_found = false;
    for artifact in output.artifacts() {
        if artifact.attempt_identity() == &draft.synthesis_attempt {
            synthesis_found = true;
        } else {
            sources.insert(artifact.artifact().digest().clone());
        }
    }
    if !synthesis_found {
        return Err(WorkflowSynthesisRefusal::UnknownSynthesisArtifact);
    }
    if sources.is_empty() {
        return Err(WorkflowSynthesisRefusal::NoSources);
    }
    if !draft.cited_source_digests.is_subset(&sources) {
        return Err(WorkflowSynthesisRefusal::UnknownCitation);
    }
    if draft.cited_source_digests != sources {
        return Err(WorkflowSynthesisRefusal::IncompleteCitations);
    }
    Ok(())
}

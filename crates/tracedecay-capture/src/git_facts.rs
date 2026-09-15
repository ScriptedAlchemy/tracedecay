use tracedecay_domain::{
    CanonicalGitEvidenceKindV1, CanonicalObservationFactV1, CanonicalWorkflowEvidenceKindV1,
};

pub(crate) fn append_diff_and_pull_request_facts(
    facts: &mut Vec<CanonicalObservationFactV1>,
    has_diffs: bool,
    pull_request_references: impl IntoIterator<Item = Option<String>>,
) {
    if has_diffs {
        facts.push(CanonicalObservationFactV1::Git {
            evidence_kind: CanonicalGitEvidenceKindV1::Diff,
            reference: None,
            content: None,
        });
    }
    for reference in pull_request_references {
        facts.push(CanonicalObservationFactV1::Git {
            evidence_kind: CanonicalGitEvidenceKindV1::PullRequest,
            reference: reference.clone(),
            content: None,
        });
        facts.push(CanonicalObservationFactV1::Workflow {
            evidence_kind: CanonicalWorkflowEvidenceKindV1::PullRequest,
            reference,
            content: None,
        });
    }
}

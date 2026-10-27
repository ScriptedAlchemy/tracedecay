//! Bounded source expansion through the current index or an immutable Git blob.

use std::io::Read;
use std::sync::Arc;
use std::time::Instant;

use tracedecay_code_index::grep_search::MAX_INTERACTIVE_SOURCE_BYTES;
use tracedecay_contracts::git::{
    GIT_HISTORICAL_BLOB_MAX_BYTES, GitHistoricalBlobRequestV1, GitIntelligenceError,
};
use tracedecay_contracts::retrieval::{
    PrimitiveFailureKind, ResultProjection, RetrievalPortContext, RetrievalPortOutcome,
    SourceLinesRequest, SourceLinesResult, SourceReference, SourceRetrievalPort,
};
use tracedecay_contracts::{
    ApplicationContractError, CoverageCompleteness, DisclosureClass, EvidenceDomain, Omission,
    OmissionReason, RequestAdmission, ResolvedScope,
};
use tracedecay_domain::{
    FileOccurrenceId, GitOidV1, RetrievalAnchorId, SourceSpan, TemporalModeV1, canonical_sha256,
};
use tracedecay_graph_query::SourceReadContext;
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::native_runtime;

use super::super::runtime::ExtendedPrimitiveFuture;
use super::{completed, evidence_unavailable, failed, now_observed, omitted_evidence, refused};
use crate::diagnostics_publication::CodeIndexPublicationIdentityPortV1;
use crate::git_intelligence::NativeGitIntelligence;

// A source expansion should fit alongside the PR brief and its provenance in
// the existing human response budget. Whole-line continuation preserves exact
// byte spans and never hides the unread suffix of a partially returned line.
const MAX_REVISION_SOURCE_LINES: u32 = 200;
const MAX_REVISION_SOURCE_BYTES: usize = 4_096;

pub struct TraceDecaySourceLinesPortV1 {
    source_runtime: Arc<SourceReadContext>,
    code_index_identity: Arc<dyn CodeIndexPublicationIdentityPortV1>,
    scope: ResolvedScope,
}

impl TraceDecaySourceLinesPortV1 {
    pub fn new(
        source_runtime: Arc<SourceReadContext>,
        code_index_identity: Arc<dyn CodeIndexPublicationIdentityPortV1>,
        scope: ResolvedScope,
    ) -> Result<Self, ApplicationContractError> {
        scope.validate()?;
        if source_runtime.project_id() != scope.project_id.as_str() {
            return Err(ApplicationContractError::Inconsistent {
                field: "source lines admitted project authority",
            });
        }
        Ok(Self {
            source_runtime,
            code_index_identity,
            scope,
        })
    }

    fn admission(
        &self,
        context: RetrievalPortContext<'_>,
    ) -> Option<RetrievalPortOutcome<SourceLinesResult>> {
        let observed_at = now_observed();
        if context.request.validate().is_err()
            || context.request.scope() != &self.scope
            || !context.request.allows(
                context.operation.capability_id(),
                context.operation.use_case_id(),
            )
        {
            return Some(source_denied());
        }
        match context.request.admission_at(observed_at) {
            RequestAdmission::Admitted => None,
            RequestAdmission::Cancelled => Some(RetrievalPortOutcome::Cancelled(omitted_evidence(
                EvidenceDomain::Source,
                observed_at,
                OmissionReason::Cancelled,
                0,
            ))),
            RequestAdmission::TimedOut => Some(RetrievalPortOutcome::TimedOut(omitted_evidence(
                EvidenceDomain::Source,
                observed_at,
                OmissionReason::TimedOut,
                0,
            ))),
            RequestAdmission::GrantExpired => Some(source_denied()),
        }
    }

    async fn indexed(
        &self,
        context: RetrievalPortContext<'_>,
        file: &FileOccurrenceId,
        span: SourceSpan,
        projection: ResultProjection,
    ) -> RetrievalPortOutcome<SourceLinesResult> {
        if span.len() > MAX_INTERACTIVE_SOURCE_BYTES {
            return unavailable(OmissionReason::Budget);
        }
        let disclose_source = match projection {
            ResultProjection::Evidence
                if context.request.grant().disclosure >= DisclosureClass::Evidence =>
            {
                true
            }
            ResultProjection::Evidence => return unavailable(OmissionReason::Redacted),
            ResultProjection::Summary | ResultProjection::ReferencesOnly => false,
        };
        let Some(identity) = self
            .code_index_identity
            .resolve_current_for_scope(
                self.source_runtime.project_root().to_path_buf(),
                context.request.scope().clone(),
            )
            .await
        else {
            return unavailable(OmissionReason::Unavailable);
        };
        let Some(relative) = identity.logical_path(file) else {
            return unavailable(OmissionReason::Stale);
        };
        let source = self.source_runtime.project_root().join(relative);
        // A current file may have grown since the indexed digest was captured.
        // Bound the read itself rather than trusting that stale metadata.
        let Ok(Ok(bytes)) = native_runtime::spawn_blocking(move || {
            let mut bytes = Vec::new();
            std::fs::File::open(source)?
                .take(MAX_INTERACTIVE_SOURCE_BYTES + 1)
                .read_to_end(&mut bytes)?;
            Ok::<_, std::io::Error>(bytes)
        })
        .await
        else {
            return unavailable(OmissionReason::Unavailable);
        };
        if bytes.len() as u64 > MAX_INTERACTIVE_SOURCE_BYTES {
            return unavailable(OmissionReason::Budget);
        }
        let Some((_, indexed_digest)) = identity.file(relative) else {
            return unavailable(OmissionReason::Stale);
        };
        if &tracedecay_code_index::intake::content_digest(&bytes) != indexed_digest {
            return unavailable(OmissionReason::Stale);
        }
        let (Ok(start), Ok(end)) = (
            usize::try_from(span.start_byte),
            usize::try_from(span.end_byte),
        ) else {
            return invalid_range();
        };
        let Some(bytes) = bytes.get(start..end) else {
            return invalid_range();
        };
        let Ok(body) = std::str::from_utf8(bytes) else {
            return invalid_range();
        };
        let Ok(digest) = canonical_sha256(&(
            "tracedecay.primitive.source-lines.v1",
            relative,
            span.start_byte,
            span.end_byte,
            bytes,
        )) else {
            return failed(EvidenceDomain::Source, now_observed());
        };
        let Ok(anchor) = RetrievalAnchorId::new(format!(
            "anchor.source-lines.{}",
            digest.as_str().trim_start_matches("sha256:")
        )) else {
            return failed(EvidenceDomain::Source, now_observed());
        };
        completed(
            SourceLinesResult::Indexed {
                file: disclose_source.then(|| relative.to_owned()),
                body: disclose_source.then(|| body.to_owned()),
                references: vec![SourceReference { anchor, span }],
            },
            EvidenceDomain::Source,
            now_observed(),
        )
    }

    async fn revision(
        &self,
        context: RetrievalPortContext<'_>,
        revision: &GitOidV1,
        file: &str,
        start_line: u32,
        end_line: u32,
    ) -> RetrievalPortOutcome<SourceLinesResult> {
        if context.request.grant().disclosure < DisclosureClass::Evidence {
            return unavailable(OmissionReason::Redacted);
        }
        if self
            .source_runtime
            .path_policy()
            .without_generated_dir_defaults()
            .excludes(file)
        {
            return source_denied();
        }
        let Some(deadline) =
            tracedecay_daemon_protocol::deadline_remaining(context.request.deadline())
                .and_then(|remaining| Instant::now().checked_add(remaining))
        else {
            return RetrievalPortOutcome::TimedOut(omitted_evidence(
                EvidenceDomain::Source,
                now_observed(),
                OmissionReason::TimedOut,
                0,
            ));
        };
        let cancel = CancellationToken::new();
        let _cancel_on_drop = CancelSourceReadOnDrop(cancel.clone());
        let git = NativeGitIntelligence::new(
            self.source_runtime.project_root().to_path_buf(),
            self.scope.repository_id.clone(),
            self.scope.worktree_id.clone(),
        )
        .with_execution_bounds(
            Some(deadline),
            Some(cancel),
            GIT_HISTORICAL_BLOB_MAX_BYTES as usize,
        );
        let request = GitHistoricalBlobRequestV1 {
            commit: revision.clone(),
            path: file.to_owned(),
            max_bytes: GIT_HISTORICAL_BLOB_MAX_BYTES,
            include_bytes: true,
        };
        // The native authority checks these controls when the queued worker
        // starts and after its bounded read. An individual gix object read is
        // indivisible; abandoning this future cancels its next checkpoint and
        // prevents that work from continuing or publishing a late result.
        let blob = match native_runtime::spawn_blocking(move || git.historical_blob(&request)).await
        {
            Ok(Ok(blob)) => blob,
            Ok(Err(GitIntelligenceError::HistoricalBlobBoundExceeded { .. })) => {
                return unavailable(OmissionReason::Budget);
            }
            Ok(Err(GitIntelligenceError::Cancelled)) => {
                return RetrievalPortOutcome::Cancelled(omitted_evidence(
                    EvidenceDomain::Source,
                    now_observed(),
                    OmissionReason::Cancelled,
                    0,
                ));
            }
            Ok(Err(GitIntelligenceError::DeadlineExceeded)) => {
                return RetrievalPortOutcome::TimedOut(omitted_evidence(
                    EvidenceDomain::Source,
                    now_observed(),
                    OmissionReason::TimedOut,
                    0,
                ));
            }
            _ => return unavailable(OmissionReason::Unavailable),
        };
        if blob.repository != self.scope.repository_id
            || blob.worktree != self.scope.worktree_id
            || &blob.commit != revision
            || blob.path != file
        {
            return source_denied();
        }
        let (Some(blob_oid), Some(bytes)) = (blob.blob_oid, blob.bytes) else {
            return source_denied();
        };
        if bytes.contains(&0) {
            return unavailable(OmissionReason::Unsupported);
        }
        let Ok(source) = std::str::from_utf8(&bytes) else {
            return unavailable(OmissionReason::Unsupported);
        };
        let (span, actual_end, next_line) = match bounded_lines(source, start_line, end_line) {
            Ok(lines) => lines,
            Err(LineReadFailure::MissingRange) => return invalid_range(),
            Err(LineReadFailure::LineExceedsBudget) => return unavailable(OmissionReason::Budget),
        };
        let Some(body) = source.get(span.start_byte as usize..span.end_byte as usize) else {
            return failed(EvidenceDomain::Source, now_observed());
        };
        let outcome = completed(
            SourceLinesResult::Revision {
                file: file.to_owned(),
                revision: revision.clone(),
                blob_oid,
                start_line,
                end_line: actual_end,
                span,
                body: body.to_owned(),
                next_line,
            },
            EvidenceDomain::Source,
            now_observed(),
        );
        let RetrievalPortOutcome::Completed(mut evidence) = outcome else {
            return outcome;
        };
        if next_line.is_none() {
            return RetrievalPortOutcome::Completed(evidence);
        }
        evidence.coverage.completeness = CoverageCompleteness::Partial;
        for domain in &mut evidence.coverage.domains {
            domain.completeness = CoverageCompleteness::Partial;
        }
        evidence.omissions.push(Omission {
            domain: EvidenceDomain::Source,
            count: 1,
            reason: OmissionReason::Budget,
        });
        RetrievalPortOutcome::Partial(evidence)
    }
}

impl SourceRetrievalPort for TraceDecaySourceLinesPortV1 {
    fn source_lines<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a SourceLinesRequest,
    ) -> ExtendedPrimitiveFuture<'a, SourceLinesResult> {
        Box::pin(tracing::Instrument::instrument(
            async move {
                let mut outcome = if let Some(outcome) = self.admission(context) {
                    outcome
                } else if request.validate().is_err() {
                    invalid_range()
                } else {
                    let outcome = match request {
                        SourceLinesRequest::Indexed { file, span, meta } => {
                            self.indexed(context, file, *span, meta.projection).await
                        }
                        SourceLinesRequest::Revision {
                            revision,
                            file,
                            start_line,
                            end_line,
                        } => {
                            self.revision(context, revision, file, *start_line, *end_line)
                                .await
                        }
                    };
                    self.admission(context).unwrap_or(outcome)
                };
                if matches!(request, SourceLinesRequest::Revision { .. }) {
                    let evidence = match &mut outcome {
                        RetrievalPortOutcome::Completed(evidence)
                        | RetrievalPortOutcome::Partial(evidence)
                        | RetrievalPortOutcome::Cancelled(evidence)
                        | RetrievalPortOutcome::TimedOut(evidence)
                        | RetrievalPortOutcome::Failed(evidence)
                        | RetrievalPortOutcome::Unavailable(evidence)
                        | RetrievalPortOutcome::Refused(evidence, _) => evidence,
                    };
                    evidence.temporal.requested_mode = TemporalModeV1::Forensic;
                }
                outcome
            },
            tracing::trace_span!("usecases.primitives.source_lines"),
        ))
    }
}

enum LineReadFailure {
    MissingRange,
    LineExceedsBudget,
}

struct CancelSourceReadOnDrop(CancellationToken);

impl Drop for CancelSourceReadOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn bounded_lines(
    source: &str,
    start_line: u32,
    end_line: u32,
) -> Result<(SourceSpan, u32, Option<u32>), LineReadFailure> {
    let mut offset = 0usize;
    let mut start = None;
    let mut end = 0usize;
    let mut actual_end = 0u32;
    let mut next_line = None;
    for (index, line) in source.split_inclusive('\n').enumerate() {
        // The native blob ceiling is smaller than u32::MAX, so every line
        // index and byte offset here fits in the public coordinate types.
        let number = index as u32 + 1;
        if number > end_line {
            break;
        }
        if number < start_line {
            offset += line.len();
            continue;
        }
        let selected_start = *start.get_or_insert(offset);
        if number - start_line >= MAX_REVISION_SOURCE_LINES
            || offset + line.len() - selected_start > MAX_REVISION_SOURCE_BYTES
        {
            if actual_end == 0 {
                return Err(LineReadFailure::LineExceedsBudget);
            }
            next_line = Some(number);
            break;
        }
        offset += line.len();
        end = offset;
        actual_end = number;
    }
    let Some(start) = start.filter(|_| actual_end > 0) else {
        return Err(LineReadFailure::MissingRange);
    };
    Ok((
        SourceSpan {
            start_byte: start as u64,
            end_byte: end as u64,
        },
        actual_end,
        next_line,
    ))
}

fn invalid_range() -> RetrievalPortOutcome<SourceLinesResult> {
    refused(
        PrimitiveFailureKind::InvalidRequest,
        "application.source-lines.invalid-range",
        "Use a valid source selector and a nonempty range within that source.",
        EvidenceDomain::Source,
        now_observed(),
    )
}

fn source_denied() -> RetrievalPortOutcome<SourceLinesResult> {
    refused(
        PrimitiveFailureKind::NotFoundOrNotAuthorized,
        "application.source-lines.not-found-or-not-authorized",
        "The source is not present or not authorized in the admitted repository.",
        EvidenceDomain::Source,
        now_observed(),
    )
}

fn unavailable(reason: OmissionReason) -> RetrievalPortOutcome<SourceLinesResult> {
    evidence_unavailable(EvidenceDomain::Source, now_observed(), reason, 1)
}

#[cfg(test)]
#[path = "source_lines_tests.rs"]
mod tests;

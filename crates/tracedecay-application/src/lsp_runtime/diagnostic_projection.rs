//! Feedback diagnostic admission and projection into LSP gateway diagnostics.

use std::collections::BTreeMap;
use std::sync::Arc;

use tracedecay_domain::DiagnosticSeverityV1;
use tracedecay_domain::feedback::{
    FeedbackCycleResultV1, FeedbackDiagnosticProducerV1, FeedbackFindingLifecycleV1,
    FeedbackFindingV1,
};
use tracedecay_lsp::{
    AdmittedRoot, DiagnosticSeverity, DiagnosticSource, GatewayDiagnostic,
    GatewayDiagnosticCoverage, GatewayDiagnosticData, GatewayDiagnosticIdentity, LspRange,
    LspRuntimeFailure, LspRuntimeFuture,
};
use tracedecay_policy::diagnostic_curation::{DiagnosticCurationDecisionV1, curate_diagnostic};

use super::context_projection::{
    cycle_coverage, finding_matches_document, gateway_diagnostic_coverage,
    gateway_diagnostic_lifecycle, gateway_diagnostic_provider_state,
};
use super::diagnostic_records::LspFeedbackDiagnosticRecordPort;
use super::{LspFeedbackProjectionScope, byte_offsets_to_utf16_range};

/// Hydrates canonical feedback finding anchors through the existing
/// diagnostics/source owner and performs exact UTF-16 projection.
pub trait LspFeedbackDiagnosticProjectionPort: Send + Sync {
    fn project(
        &self,
        root: AdmittedRoot,
        document_uri: String,
        scope: LspFeedbackProjectionScope,
        cycle: FeedbackCycleResultV1,
        expansion_handles: BTreeMap<String, String>,
    ) -> LspRuntimeFuture<Result<Vec<GatewayDiagnostic>, LspRuntimeFailure>>;
}

/// Canonical source identity and text needed to project byte-addressed
/// generation diagnostics into negotiated UTF-16 LSP ranges.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspFeedbackDocumentSnapshot {
    pub text: String,
}

pub trait LspFeedbackDocumentSnapshotPort: Send + Sync {
    fn snapshot(
        &self,
        root: AdmittedRoot,
        document_uri: String,
    ) -> LspRuntimeFuture<Result<LspFeedbackDocumentSnapshot, LspRuntimeFailure>>;
}

/// Why one feedback finding did not become a published LSP diagnostic.
///
/// Projection refusals used to be anonymous `continue`s, so an empty Problems
/// list was indistinguishable from "the store never had the record".
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum FeedbackDiagnosticProjectionSkipV1 {
    /// The finding is not in the active lifecycle state.
    LifecycleNotActive,
    /// The finding carries no `RetrievalAnchorId`, so no durable record can
    /// be addressed.
    NoRetrievalAnchor,
    /// The anchor resolves to no record: the producing pillar never published
    /// this finding into the diagnostics store.
    AnchorNotPublished,
    /// The record attaches to a different file than the cycle's impact target.
    ImpactTargetFileMismatch,
    /// The cycle carries no impact target to compare the record's file with.
    ImpactTargetAbsent,
    /// The finding belongs to another file than the admitted document.
    DocumentFileMismatch,
    /// The record belongs to a different clean generation.
    GenerationMismatch,
    /// The record was collected against different file content.
    ContentDigestMismatch,
    /// The record is superseded or cleared rather than current.
    RecordNotCurrent,
    /// The record names a different source revision than the cycle scope.
    SourceRevisionDrift,
}

impl FeedbackDiagnosticProjectionSkipV1 {
    /// Stable classification label for logs and typed status projections.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::LifecycleNotActive => "lifecycle-not-active",
            Self::NoRetrievalAnchor => "no-retrieval-anchor",
            Self::AnchorNotPublished => "anchor-not-published",
            Self::ImpactTargetFileMismatch => "impact-target-file-mismatch",
            Self::ImpactTargetAbsent => "impact-target-absent",
            Self::DocumentFileMismatch => "document-file-mismatch",
            Self::GenerationMismatch => "generation-mismatch",
            Self::ContentDigestMismatch => "content-digest-mismatch",
            Self::RecordNotCurrent => "record-not-current",
            Self::SourceRevisionDrift => "source-revision-drift",
        }
    }
}

/// Records one typed projection refusal. Refusals are observable rather than
/// silent so an empty Problems list can be attributed to a cause.
pub(super) fn skipped(finding_id: &str, skip: FeedbackDiagnosticProjectionSkipV1) {
    tracing::debug!(
        target: "tracedecay::lsp::diagnostics",
        finding_id,
        reason = skip.label(),
        "feedback finding was not projected as an LSP diagnostic"
    );
}

/// Decides whether a resolved durable record may be projected for this cycle.
///
/// Pure and total: every refusal is named.
pub fn classify_feedback_diagnostic_admission(
    record: &tracedecay_domain::GenerationDiagnosticV1,
    impact_target_file: Option<&tracedecay_domain::FileOccurrenceId>,
    code_generation_id: &tracedecay_domain::CodeGenerationId,
    document_content_digest: &tracedecay_domain::ContentDigest,
    head_commit_id: &tracedecay_domain::CommitId,
) -> Result<(), FeedbackDiagnosticProjectionSkipV1> {
    let Some(target_file) = impact_target_file else {
        return Err(FeedbackDiagnosticProjectionSkipV1::ImpactTargetAbsent);
    };
    match curate_diagnostic(
        record,
        target_file,
        code_generation_id,
        document_content_digest,
        head_commit_id,
    ) {
        DiagnosticCurationDecisionV1::Admit => Ok(()),
        DiagnosticCurationDecisionV1::TargetFileMismatch => {
            Err(FeedbackDiagnosticProjectionSkipV1::ImpactTargetFileMismatch)
        }
        DiagnosticCurationDecisionV1::GenerationMismatch => {
            Err(FeedbackDiagnosticProjectionSkipV1::GenerationMismatch)
        }
        DiagnosticCurationDecisionV1::ContentDigestMismatch => {
            Err(FeedbackDiagnosticProjectionSkipV1::ContentDigestMismatch)
        }
        DiagnosticCurationDecisionV1::RecordNotCurrent => {
            Err(FeedbackDiagnosticProjectionSkipV1::RecordNotCurrent)
        }
        DiagnosticCurationDecisionV1::SourceRevisionDrift => {
            Err(FeedbackDiagnosticProjectionSkipV1::SourceRevisionDrift)
        }
    }
}

pub(super) fn gateway_diagnostic_data(
    finding: &FeedbackFindingV1,
    anchor: &tracedecay_domain::RetrievalAnchorId,
    scope: &LspFeedbackProjectionScope,
    coverage: GatewayDiagnosticCoverage,
    expansion_handles: &BTreeMap<String, String>,
) -> Option<GatewayDiagnosticData> {
    let document_content_digest = scope.document_content_digest.as_ref()?;
    let expansion_handle = expansion_handles.get(finding.finding_id.as_str())?;
    Some(GatewayDiagnosticData {
        identity: GatewayDiagnosticIdentity {
            finding_id: finding.finding_id.as_str().to_owned(),
            anchor_id: anchor.as_str().to_owned(),
            generation: scope.generation,
            head_commit_id: scope.head_commit_id.as_str().to_owned(),
            code_generation_id: scope.code_generation_id.as_str().to_owned(),
            snapshot_digest: scope.snapshot_digest.as_str().to_owned(),
            invalidation_digest: scope.invalidation_digest.as_str().to_owned(),
            snapshot_content_digest: scope.snapshot_content_digest.as_str().to_owned(),
            document_content_digest: document_content_digest.as_str().to_owned(),
        },
        lifecycle: gateway_diagnostic_lifecycle(finding.lifecycle),
        provider_state: gateway_diagnostic_provider_state(finding.provider_state),
        coverage,
        expansion_handle: expansion_handle.clone(),
    })
}

pub(super) const fn gateway_severity(severity: DiagnosticSeverityV1) -> DiagnosticSeverity {
    match severity {
        DiagnosticSeverityV1::Error => DiagnosticSeverity::Error,
        DiagnosticSeverityV1::Warning => DiagnosticSeverity::Warning,
        DiagnosticSeverityV1::Information => DiagnosticSeverity::Information,
        DiagnosticSeverityV1::Hint => DiagnosticSeverity::Hint,
    }
}

pub(super) const fn advisory_diagnostic_source(
    producer: tracedecay_domain::feedback::FeedbackDiagnosticProducerV1,
) -> DiagnosticSource {
    match producer {
        FeedbackDiagnosticProducerV1::GitHubReview => DiagnosticSource::TraceDecayGitHub,
        FeedbackDiagnosticProducerV1::CiLocalization => DiagnosticSource::TraceDecayCi,
        FeedbackDiagnosticProducerV1::Proximity => DiagnosticSource::TraceDecayProximity,
    }
}

/// Real finding-anchor hydration over the canonical managed diagnostics store.
pub struct DiagnosticsStoreLspFeedbackProjection<S> {
    records: Arc<dyn LspFeedbackDiagnosticRecordPort>,
    documents: Arc<S>,
}

impl<S> DiagnosticsStoreLspFeedbackProjection<S> {
    pub fn new(records: Arc<dyn LspFeedbackDiagnosticRecordPort>, documents: Arc<S>) -> Self {
        Self { records, documents }
    }
}

impl<S> LspFeedbackDiagnosticProjectionPort for DiagnosticsStoreLspFeedbackProjection<S>
where
    S: LspFeedbackDocumentSnapshotPort + 'static,
{
    fn project(
        &self,
        root: AdmittedRoot,
        document_uri: String,
        scope: LspFeedbackProjectionScope,
        cycle: FeedbackCycleResultV1,
        expansion_handles: BTreeMap<String, String>,
    ) -> LspRuntimeFuture<Result<Vec<GatewayDiagnostic>, LspRuntimeFailure>> {
        let records = Arc::clone(&self.records);
        let documents = Arc::clone(&self.documents);
        Box::pin(hotpath::future!(
            async move {
                let document = documents.snapshot(root, document_uri.clone()).await?;
                let Some(document_content_digest) = scope.document_content_digest.as_ref() else {
                    return Err(LspRuntimeFailure::new(
                        "diagnostic-document-identity-unavailable",
                    ));
                };
                if tracedecay_code_index::intake::content_digest(document.text.as_bytes())
                    != *document_content_digest
                {
                    return Err(LspRuntimeFailure::new("diagnostic-document-content-stale"));
                }
                let coverage = gateway_diagnostic_coverage(cycle_coverage(&cycle));
                let mut diagnostics = Vec::new();
                let impact_target_file = cycle.impact.as_ref().map(|impact| &impact.target.file);
                for finding in &cycle.findings {
                    let finding_id = finding.finding_id.as_str();
                    if !finding_matches_document(finding, &scope, impact_target_file) {
                        skipped(
                            finding_id,
                            FeedbackDiagnosticProjectionSkipV1::DocumentFileMismatch,
                        );
                        continue;
                    }
                    if finding.lifecycle != FeedbackFindingLifecycleV1::Active {
                        skipped(
                            finding_id,
                            FeedbackDiagnosticProjectionSkipV1::LifecycleNotActive,
                        );
                        continue;
                    }
                    let Some(anchor) = finding.retrieval_anchor_id.as_ref() else {
                        skipped(
                            finding_id,
                            FeedbackDiagnosticProjectionSkipV1::NoRetrievalAnchor,
                        );
                        continue;
                    };
                    if let Some(projection) = finding.diagnostic_projection.as_ref() {
                        let Some(target_file) = impact_target_file else {
                            skipped(
                                finding_id,
                                FeedbackDiagnosticProjectionSkipV1::ImpactTargetAbsent,
                            );
                            continue;
                        };
                        if projection.file != *target_file {
                            skipped(
                                finding_id,
                                FeedbackDiagnosticProjectionSkipV1::ImpactTargetFileMismatch,
                            );
                            continue;
                        }
                        let start = usize::try_from(projection.span.start_byte)
                            .map_err(|_| LspRuntimeFailure::new("diagnostic-span-invalid"))?;
                        let end = usize::try_from(projection.span.end_byte)
                            .map_err(|_| LspRuntimeFailure::new("diagnostic-span-invalid"))?;
                        let (start, end) = byte_offsets_to_utf16_range(&document.text, start, end)?;
                        diagnostics.push(GatewayDiagnostic {
                            uri: document_uri.clone(),
                            range: LspRange { start, end },
                            severity: Some(gateway_severity(projection.severity)),
                            code: Some(projection.code.clone()),
                            code_description_uri: projection.code_description_uri.clone(),
                            message: projection.safe_bounded_message.clone(),
                            source: advisory_diagnostic_source(projection.producer),
                            related_information: Vec::new(),
                            data: gateway_diagnostic_data(
                                finding,
                                anchor,
                                &scope,
                                coverage,
                                &expansion_handles,
                            ),
                        });
                        continue;
                    }
                    let Some(record) = records.diagnostic_by_anchor(anchor.clone()).await? else {
                        skipped(
                            finding_id,
                            FeedbackDiagnosticProjectionSkipV1::AnchorNotPublished,
                        );
                        continue;
                    };
                    if let Err(skip) = classify_feedback_diagnostic_admission(
                        &record,
                        impact_target_file,
                        &scope.code_generation_id,
                        document_content_digest,
                        &cycle.scope.head_commit_id,
                    ) {
                        skipped(finding_id, skip);
                        continue;
                    }
                    let start = usize::try_from(record.span.start_byte)
                        .map_err(|_| LspRuntimeFailure::new("diagnostic-span-invalid"))?;
                    let end = usize::try_from(record.span.end_byte)
                        .map_err(|_| LspRuntimeFailure::new("diagnostic-span-invalid"))?;
                    let (start, end) = byte_offsets_to_utf16_range(&document.text, start, end)?;
                    let data = gateway_diagnostic_data(
                        finding,
                        anchor,
                        &scope,
                        coverage,
                        &expansion_handles,
                    );
                    diagnostics.push(GatewayDiagnostic {
                        uri: document_uri.clone(),
                        range: LspRange { start, end },
                        severity: Some(gateway_severity(record.severity)),
                        code: Some(record.code),
                        code_description_uri: None,
                        message: record.message,
                        // Name the real producer instead of an anonymous
                        // `tracedecay` lane (Plan 35).
                        source: DiagnosticSource::from_producer(
                            record.provenance.producer.as_str(),
                        ),
                        related_information: Vec::new(),
                        data,
                    });
                }
                Ok(diagnostics)
            },
            label = "usecases.lsp.diagnostics.project"
        ))
    }
}

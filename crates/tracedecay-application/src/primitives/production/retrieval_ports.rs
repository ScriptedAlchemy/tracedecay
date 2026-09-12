//! Source-line and operational-health retrieval ports.

use std::sync::Arc;

use tracedecay_code_index::grep_search::MAX_INTERACTIVE_SOURCE_BYTES;
use tracedecay_contracts::retrieval::{
    HealthReadRequest, HealthReadResult, OperationalRetrievalPort, ResultProjection,
    RetrievalPortContext, RetrievalPortOutcome, SourceLinesRequest, SourceLinesResult,
    SourceReference, SourceRetrievalPort,
};
use tracedecay_contracts::{DisclosureClass, EvidenceDomain, OmissionReason};
use tracedecay_domain::{RetrievalAnchorId, canonical_sha256};
use tracedecay_graph_query::SourceReadContext;

use super::super::runtime::ExtendedPrimitiveFuture;
use super::{completed, evidence_unavailable, failed, now_observed};
use crate::diagnostics_publication::CodeIndexPublicationIdentityPortV1;

pub struct TraceDecaySourceLinesPortV1 {
    source_runtime: Arc<SourceReadContext>,
    code_index_identity: Arc<dyn CodeIndexPublicationIdentityPortV1>,
}

impl TraceDecaySourceLinesPortV1 {
    pub fn new(
        source_runtime: Arc<SourceReadContext>,
        code_index_identity: Arc<dyn CodeIndexPublicationIdentityPortV1>,
    ) -> Self {
        Self {
            source_runtime,
            code_index_identity,
        }
    }
}

impl SourceRetrievalPort for TraceDecaySourceLinesPortV1 {
    fn source_lines<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a SourceLinesRequest,
    ) -> ExtendedPrimitiveFuture<'a, SourceLinesResult> {
        Box::pin(hotpath::future!(
            async move {
                let finished_at = now_observed();
                let unavailable =
                    |reason| evidence_unavailable(EvidenceDomain::Source, finished_at, reason, 0);
                if request.span.validate().is_err()
                    || request.span.len() > MAX_INTERACTIVE_SOURCE_BYTES
                {
                    return failed(EvidenceDomain::Source, finished_at);
                }
                let disclose_source = match request.meta.projection {
                    ResultProjection::Evidence
                        if context.request.grant().disclosure >= DisclosureClass::Evidence =>
                    {
                        true
                    }
                    ResultProjection::Evidence => {
                        return unavailable(OmissionReason::Redacted);
                    }
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
                let Some(relative) = identity.logical_path(&request.file) else {
                    return unavailable(OmissionReason::Stale);
                };
                let Ok(bytes) =
                    tokio::fs::read(self.source_runtime.project_root().join(relative)).await
                else {
                    return unavailable(OmissionReason::Unavailable);
                };
                let Some((_, indexed_digest)) = identity.file(relative) else {
                    return unavailable(OmissionReason::Stale);
                };
                if &tracedecay_code_index::intake::content_digest(&bytes) != indexed_digest {
                    return unavailable(OmissionReason::Stale);
                }
                let (Ok(start), Ok(end)) = (
                    usize::try_from(request.span.start_byte),
                    usize::try_from(request.span.end_byte),
                ) else {
                    return failed(EvidenceDomain::Source, finished_at);
                };
                if end > bytes.len() || start > end {
                    return failed(EvidenceDomain::Source, finished_at);
                }
                let Ok(body) = std::str::from_utf8(&bytes[start..end]) else {
                    return failed(EvidenceDomain::Source, finished_at);
                };
                let Ok(digest) = canonical_sha256(&(
                    "tracedecay.primitive.source-lines.v1",
                    relative,
                    request.span.start_byte,
                    request.span.end_byte,
                    &bytes[start..end],
                )) else {
                    return failed(EvidenceDomain::Source, finished_at);
                };
                let Ok(anchor) = RetrievalAnchorId::new(format!(
                    "anchor.source-lines.{}",
                    digest.as_str().trim_start_matches("sha256:")
                )) else {
                    return failed(EvidenceDomain::Source, finished_at);
                };
                completed(
                    SourceLinesResult {
                        file: disclose_source.then(|| relative.to_owned()),
                        body: disclose_source.then(|| body.to_owned()),
                        references: vec![SourceReference {
                            anchor,
                            span: request.span,
                        }],
                    },
                    EvidenceDomain::Source,
                    finished_at,
                )
            },
            label = "usecases.primitives.source_lines"
        ))
    }
}

pub struct TraceDecayHealthPortV1 {
    source_runtime: Arc<SourceReadContext>,
}

impl TraceDecayHealthPortV1 {
    pub fn new(source_runtime: Arc<SourceReadContext>) -> Self {
        Self { source_runtime }
    }
}

impl OperationalRetrievalPort for TraceDecayHealthPortV1 {
    fn health_read(
        &self,
        _context: &RetrievalPortContext<'_>,
        _request: &HealthReadRequest,
    ) -> RetrievalPortOutcome<HealthReadResult> {
        let serving_db_exists = self.source_runtime.db().canonical_database_path().is_file();
        let status = if serving_db_exists && !self.source_runtime.is_read_only() {
            "ok"
        } else if serving_db_exists {
            "read_only"
        } else {
            "degraded"
        };
        completed(
            HealthReadResult {
                status: status.to_owned(),
            },
            EvidenceDomain::Operational,
            now_observed(),
        )
    }
}

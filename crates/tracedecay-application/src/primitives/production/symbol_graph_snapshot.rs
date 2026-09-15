//! Symbol-graph cursor snapshot authority for one project.

use std::path::PathBuf;
use std::sync::Arc;

use tracedecay_contracts::{RequestContext, ResolvedScope};
use tracedecay_domain::{
    CommitId, ManifestDigest, RetrievalGrainV1, SessionId, SignedCursorKeyRefV1, TemporalModeV1,
    UtcMicros, canonical_sha256,
};
use tracedecay_temporal_query::ports::{
    BindingDigest, KernelVersions, TemporalExecutionSnapshot, TemporalSnapshotRequest,
    TemporalWatermarks,
};
use tracedecay_temporal_query::resolution::ValidatedAuthorization;

use super::super::concrete::{SymbolGraphCursorSnapshot, SymbolGraphCursorSnapshotAuthority};
use crate::lsp_runtime::LspCodeIndexProjectionIdentityPort;

/// Derives symbol-graph cursor snapshots from the code index's *current*
/// published generation.
///
/// The generation is resolved per call rather than captured when the runtime
/// mounts. A cached watermark would give two different graph states one cursor
/// identity, so a cursor minted before a publication would keep verifying
/// against the rows that replaced it — the page-set would change underneath the
/// caller with nothing in the answer saying so.
pub struct ProjectSymbolGraphCursorSnapshotAuthority {
    pub(super) key: SignedCursorKeyRefV1,
    pub(super) configuration_digest: ManifestDigest,
    pub(super) project_root: PathBuf,
    pub(super) scope: ResolvedScope,
    pub(super) code_index: Arc<dyn LspCodeIndexProjectionIdentityPort>,
}

pub(super) fn symbol_graph_snapshot_failure(
    code: &str,
    message: &str,
) -> tracedecay_contracts::retrieval::PrimitiveFailure {
    tracedecay_contracts::retrieval::PrimitiveFailure::new(
        tracedecay_contracts::retrieval::PrimitiveFailureKind::Unavailable,
        code,
        message,
    )
    .unwrap_or_else(|_| panic!("static"))
}

impl SymbolGraphCursorSnapshotAuthority for ProjectSymbolGraphCursorSnapshotAuthority {
    fn snapshot<'a>(
        &'a self,
        context: &'a RequestContext,
        lane: &'a str,
        _observed_at: UtcMicros,
    ) -> super::super::concrete::SymbolGraphCursorSnapshotFuture<'a> {
        Box::pin(async move {
            let graph_identity = self
                .code_index
                .current_identity(self.project_root.clone(), None)
                .await
                .map_err(|failure| {
                    symbol_graph_snapshot_failure(
                        "application.symbol-graph.identity",
                        &format!(
                            "could not read the current symbol-graph identity: {}",
                            failure.class()
                        ),
                    )
                })?
                .admit_worktree_scope(&self.scope)
                .map_err(|failure| {
                    symbol_graph_snapshot_failure(
                        "application.symbol-graph.scope",
                        &format!(
                            "the current symbol-graph identity was not admitted \
                             for the scope: {}",
                            failure.class()
                        ),
                    )
                })?;
            let code_generation_id = graph_identity.code_generation_id.clone();
            // Every component of the published generation's address folds into
            // the identity, so any republication — even one that leaves the
            // generation sequence alone — produces a different snapshot and
            // therefore refuses cursors minted before it. A dirty worktree
            // seals no commit, so the revision rides along as an option: the
            // generation and content digests already distinguish its rows.
            //
            // Where each part is bound decides how a refusal is *typed*, and
            // the cursor codec checks the request binding before the
            // watermarks. Binding the generation into the request digest would
            // therefore report an ordinary publication as a wrong request —
            // indistinguishable from a forged cursor — so the generation rides
            // the watermarks (a mismatch there is already typed stale) and only
            // the finer content digests ride the configuration binding, which
            // is checked last and so speaks only for a republication that left
            // the generation sequence unmoved.
            let graph_snapshot_digest = canonical_sha256(&(
                "tracedecay.symbol-graph.snapshot.v1",
                graph_identity
                    .source_revision
                    .as_ref()
                    .map(CommitId::as_str),
                graph_identity.code_generation_id.as_str(),
                graph_identity.snapshot_digest.as_str(),
                graph_identity.invalidation_digest.as_str(),
                graph_identity.snapshot_content_digest.as_str(),
            ))
            .map_err(|_| {
                symbol_graph_snapshot_failure(
                    "application.symbol-graph.identity",
                    "could not derive the current symbol-graph snapshot digest",
                )
            })?;
            // The snapshot identity is what a cursor is verified against on the
            // next request, so it is derived from the authorization and lane that
            // must still hold at resume time. A per-request correlation id would
            // both fail the digest binding and make every resume a different
            // request.
            let request_digest = canonical_sha256(&(
                "tracedecay.symbol-graph.cursor.v1",
                context.actor(),
                context.grant().revision,
                &context.grant().digest,
                &context.grant().issuer,
                &context.grant().allowed_capabilities,
                &context.grant().allowed_use_cases,
                context.grant().disclosure,
                lane,
            ))
            .map_err(|_| {
                symbol_graph_snapshot_failure(
                    "application.symbol-graph.request",
                    "could not derive the symbol-graph cursor request digest",
                )
            })?;
            let request = TemporalSnapshotRequest::new(
                SessionId::new("session.daemon.primitive").map_err(|_| {
                    symbol_graph_snapshot_failure(
                        "application.symbol-graph.session",
                        "could not mint primitive session id",
                    )
                })?,
                context.scope().scope_digest.as_str(),
                request_digest.as_str(),
                context.grant().digest.as_str(),
                TemporalModeV1::Current,
                RetrievalGrainV1::Occurrence,
            )
            .map_err(|_| {
                symbol_graph_snapshot_failure(
                    "application.symbol-graph.snapshot",
                    "could not build temporal snapshot request",
                )
            })?;
            let watermark = graph_identity.generation.max(1);
            let temporal = TemporalExecutionSnapshot::new_authorized(
                request,
                TemporalWatermarks {
                    generation: 1,
                    source: watermark,
                    projection: watermark,
                    index: watermark,
                    summary: watermark,
                },
                KernelVersions {
                    schema: 1,
                    ranking: 1,
                    configuration_digest: BindingDigest::new(
                        "configuration_digest",
                        canonical_sha256(&(
                            "tracedecay.symbol-graph.cursor-configuration.v1",
                            self.configuration_digest.as_str(),
                            graph_snapshot_digest.as_str(),
                            code_generation_id.as_str(),
                        ))
                        .map_err(|_| {
                            symbol_graph_snapshot_failure(
                                "application.symbol-graph.configuration",
                                "could not bind the symbol-graph snapshot configuration",
                            )
                        })?
                        .as_str(),
                    )
                    .map_err(|_| {
                        symbol_graph_snapshot_failure(
                            "application.symbol-graph.configuration",
                            "invalid configuration digest",
                        )
                    })?,
                },
                Some(self.key.clone()),
                ValidatedAuthorization::Authorized,
            )
            .map_err(|_| {
                symbol_graph_snapshot_failure(
                    "application.symbol-graph.snapshot",
                    "could not authorize temporal snapshot",
                )
            })?;
            Ok(SymbolGraphCursorSnapshot::new(temporal, code_generation_id))
        })
    }
}

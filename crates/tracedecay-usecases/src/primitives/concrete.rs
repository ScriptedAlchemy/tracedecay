use std::sync::Arc;

use tracedecay_application::retrieval::{
    PrimitiveFailure, PrimitiveFailureKind, SourceReadModeV1, SourceReadPortContext,
    SourceReadPortFuture, SourceReadPortOutcome, SourceReadPrimitivePort,
    SourceReadPrimitiveRequest,
};
use tracedecay_application::{
    ApplicationContractError, OpaqueCursor, OperationBudgetUsage, RequestAdmission, RequestContext,
    ResolvedScope,
};
use tracedecay_domain::UtcMicros;

use super::symbol_graph::SymbolGraphCursorPort;
use crate::context::read_modes::{LineRange, ReadMode};
use crate::context::source_read::{SourceReadRequest, read_source};
use crate::tracedecay::TraceDecay;
use tracedecay_temporal_query::cursor::{CursorError, StableSortKey, encode_cursor, verify_cursor};
use tracedecay_temporal_query::ports::{SessionCursorAuthenticator, TemporalExecutionSnapshot};

/// Production source-read adapter for one typed project root.
///
/// It reuses the existing path resolver, source decoder, read modes, symbol
/// projection, and cross-session cache. The typed scope is retained as the
/// extension seam for PR15; this adapter intentionally admits exactly one
/// project/repository/worktree scope.
pub struct Pr12SourceReadAdapter {
    graph: Arc<TraceDecay>,
    scope: ResolvedScope,
}

impl Pr12SourceReadAdapter {
    pub fn new(
        graph: Arc<TraceDecay>,
        scope: ResolvedScope,
    ) -> Result<Self, ApplicationContractError> {
        scope.validate()?;
        Ok(Self { graph, scope })
    }
}

impl SourceReadPrimitivePort for Pr12SourceReadAdapter {
    fn source_read<'a>(
        &'a self,
        context: SourceReadPortContext<'a>,
        request: &'a SourceReadPrimitiveRequest,
    ) -> SourceReadPortFuture<'a> {
        Box::pin(async move {
            if context.request.scope() != &self.scope || request.validate().is_err() {
                return source_read_failed(context.observed_at);
            }
            match self.read(request).await {
                Ok(result) => SourceReadPortOutcome::Completed {
                    result,
                    finished_at: context.observed_at,
                    budget: OperationBudgetUsage::default(),
                },
                Err(()) => source_read_failed(context.observed_at),
            }
        })
    }
}

impl Pr12SourceReadAdapter {
    async fn read(
        &self,
        request: &SourceReadPrimitiveRequest,
    ) -> Result<tracedecay_application::retrieval::SourceReadResultV1, ()> {
        let mode = source_read_mode(request.mode);
        let line_range = match request.mode {
            SourceReadModeV1::Lines => Some(
                request
                    .lines
                    .as_deref()
                    .and_then(LineRange::parse)
                    .ok_or(())?,
            ),
            SourceReadModeV1::Full | SourceReadModeV1::Map | SourceReadModeV1::Signatures => None,
        };
        let output = read_source(
            self.graph.as_ref(),
            SourceReadRequest {
                file: &request.file,
                mode,
                line_range,
                raw_lines: request.lines.as_deref(),
                include_symbols: request.include_symbols,
                project_id: self.scope.project_id.as_str(),
            },
        )
        .await
        .map_err(|_| ())?;
        Ok(tracedecay_application::retrieval::SourceReadResultV1 {
            file: output.file,
            mode: request.mode,
            mtime_ns: u64::try_from(output.mtime_ns).map_err(|_| ())?,
            digest: output.digest,
            token_count: output.token_count as usize,
            unchanged: output.unchanged,
            body: output.body,
            context: output.context,
        })
    }
}

const fn source_read_mode(mode: SourceReadModeV1) -> ReadMode {
    match mode {
        SourceReadModeV1::Full => ReadMode::Full,
        SourceReadModeV1::Lines => ReadMode::Lines,
        SourceReadModeV1::Map => ReadMode::Map,
        SourceReadModeV1::Signatures => ReadMode::Signatures,
    }
}

fn source_read_failed(observed_at: UtcMicros) -> SourceReadPortOutcome {
    SourceReadPortOutcome::Failed {
        finished_at: observed_at,
        budget: OperationBudgetUsage::default(),
    }
}

/// Supplies the existing query snapshot identity used by the authenticated
/// temporal cursor codec. Implementations must derive the snapshot from the
/// current request scope, grant/access binding, lane, and graph watermark.
pub trait SymbolGraphCursorSnapshotAuthority: Send + Sync {
    fn snapshot(
        &self,
        context: &RequestContext,
        lane: &str,
        observed_at: UtcMicros,
    ) -> Result<TemporalExecutionSnapshot, PrimitiveFailure>;
}

/// Bridges symbol-graph paging to the existing authenticated query cursor.
///
/// No key material, MAC, wire format, expiry, scope identity, or watermark
/// logic is defined here. Those remain owned by the supplied temporal snapshot
/// authority and [`SessionCursorAuthenticator`].
pub struct AuthenticatedSymbolGraphCursorAdapter<S: ?Sized, A: ?Sized> {
    snapshots: Arc<S>,
    authenticator: Arc<A>,
}

impl<S: ?Sized, A: ?Sized> AuthenticatedSymbolGraphCursorAdapter<S, A> {
    pub fn new(snapshots: Arc<S>, authenticator: Arc<A>) -> Self {
        Self {
            snapshots,
            authenticator,
        }
    }
}

impl<S, A> SymbolGraphCursorPort for AuthenticatedSymbolGraphCursorAdapter<S, A>
where
    S: SymbolGraphCursorSnapshotAuthority + ?Sized,
    A: SessionCursorAuthenticator + Send + Sync + ?Sized,
{
    fn resume_offset(
        &self,
        context: &RequestContext,
        lane: &str,
        cursor: &OpaqueCursor,
        observed_at: UtcMicros,
    ) -> Result<usize, PrimitiveFailure> {
        reauthorize_cursor_context(context, observed_at)?;
        let snapshot = self.snapshots.snapshot(context, lane, observed_at)?;
        validate_cursor_snapshot(context, &snapshot)?;
        let sort_key = verify_cursor(cursor.as_str(), &snapshot, self.authenticator.as_ref())
            .map_err(cursor_verification_failure)?;
        if sort_key.stable_id != lane
            || sort_key.normalized_score_micros
                > u64::try_from(sort_key.knowledge_at_micros).unwrap_or_default()
        {
            return Err(invalid_cursor());
        }
        usize::try_from(sort_key.normalized_score_micros).map_err(|_| invalid_cursor())
    }

    fn issue_cursor(
        &self,
        context: &RequestContext,
        lane: &str,
        next_offset: usize,
        total: usize,
        observed_at: UtcMicros,
    ) -> Result<OpaqueCursor, PrimitiveFailure> {
        reauthorize_cursor_context(context, observed_at)?;
        if next_offset > total || lane.is_empty() || lane.chars().any(char::is_control) {
            return Err(invalid_cursor());
        }
        let snapshot = self.snapshots.snapshot(context, lane, observed_at)?;
        validate_cursor_snapshot(context, &snapshot)?;
        let sort_key = StableSortKey {
            normalized_score_micros: u64::try_from(next_offset).map_err(|_| invalid_cursor())?,
            knowledge_at_micros: i64::try_from(total).map_err(|_| invalid_cursor())?,
            stable_id: lane.to_owned(),
        };
        let encoded = encode_cursor(&snapshot, &sort_key, self.authenticator.as_ref())
            .map_err(cursor_issue_failure)?;
        OpaqueCursor::new(encoded).map_err(|_| {
            primitive_failure(
                PrimitiveFailureKind::Unavailable,
                "application.symbol-graph.cursor-too-large",
                "the authenticated cursor exceeded the application cursor bound",
            )
        })
    }
}

fn reauthorize_cursor_context(
    context: &RequestContext,
    observed_at: UtcMicros,
) -> Result<(), PrimitiveFailure> {
    if context.validate().is_err()
        || context.admission_at(observed_at) != RequestAdmission::Admitted
    {
        return Err(primitive_failure(
            PrimitiveFailureKind::NotFoundOrNotAuthorized,
            "application.symbol-graph.cursor-not-authorized",
            "the cursor is not available for this request",
        ));
    }
    Ok(())
}

fn validate_cursor_snapshot(
    context: &RequestContext,
    snapshot: &TemporalExecutionSnapshot,
) -> Result<(), PrimitiveFailure> {
    if snapshot.root_digest().as_str() != context.scope().scope_digest.as_str()
        || snapshot.access_digest().as_str() != context.grant().digest.as_str()
    {
        return Err(primitive_failure(
            PrimitiveFailureKind::NotFoundOrNotAuthorized,
            "application.symbol-graph.cursor-not-authorized",
            "the cursor is not available for this request",
        ));
    }
    Ok(())
}

fn cursor_verification_failure(error: CursorError) -> PrimitiveFailure {
    match error {
        CursorError::GenerationMismatch
        | CursorError::ParticipantManifestMismatch
        | CursorError::EpochMismatch
        | CursorError::SourceWatermarkMismatch
        | CursorError::ProjectionWatermarkMismatch
        | CursorError::IndexWatermarkMismatch
        | CursorError::SummaryWatermarkMismatch => primitive_failure(
            PrimitiveFailureKind::Stale,
            "application.symbol-graph.cursor-stale",
            "the cursor snapshot is no longer current",
        ),
        CursorError::KeyUnavailable | CursorError::InvalidKeyMaterial => {
            cursor_issue_failure(error)
        }
        CursorError::Malformed
        | CursorError::Tampered
        | CursorError::Expired
        | CursorError::UnknownOrExpiredKey
        | CursorError::WrongRequest
        | CursorError::FilterMismatch
        | CursorError::RootMismatch
        | CursorError::SessionMismatch
        | CursorError::WrongAccess
        | CursorError::TemporalModeMismatch
        | CursorError::GrainMismatch
        | CursorError::SchemaMismatch
        | CursorError::RankingMismatch
        | CursorError::ConfigurationMismatch
        | CursorError::KeyIdMismatch
        | CursorError::KeyVersionMismatch
        | CursorError::SortKeyMismatch => primitive_failure(
            PrimitiveFailureKind::NotFoundOrNotAuthorized,
            "application.symbol-graph.cursor-not-authorized",
            "the cursor is not available for this request",
        ),
    }
}

fn cursor_issue_failure(_error: CursorError) -> PrimitiveFailure {
    primitive_failure(
        PrimitiveFailureKind::Unavailable,
        "application.symbol-graph.cursor-unavailable",
        "the authenticated cursor authority is unavailable",
    )
}

fn invalid_cursor() -> PrimitiveFailure {
    primitive_failure(
        PrimitiveFailureKind::InvalidRequest,
        "application.symbol-graph.cursor-invalid",
        "the cursor continuation is invalid",
    )
}

fn primitive_failure(
    kind: PrimitiveFailureKind,
    code: &'static str,
    message: &'static str,
) -> PrimitiveFailure {
    PrimitiveFailure::new(kind, code, message)
        .unwrap_or_else(|_| panic!("static primitive failure is valid"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use tracedecay_application::{
        ApplicationOperation, CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot,
        Deadline, DisclosureClass, RequestContext, RequestId, ResolvedScope, ResultContractRef,
    };
    use tracedecay_domain::{
        ActorId, ManifestDigest, ProjectId, RefId, RepositoryId, RetrievalGrainV1,
        SessionCursorKeyIdV1, SessionCursorVersionV1, SessionId, SignedCursorKeyRefV1,
        TemporalModeV1, UtcMicros, WorktreeId,
    };
    use tracedecay_tool_catalog::{CapabilityId, SchemaId, UseCaseId};

    use super::{AuthenticatedSymbolGraphCursorAdapter, SymbolGraphCursorSnapshotAuthority};
    use crate::primitives::SymbolGraphCursorPort;
    use tracedecay_temporal_query::ports::{
        BindingDigest, InMemoryCursorAuthenticator, KernelVersions, TemporalExecutionSnapshot,
        TemporalSnapshotRequest, TemporalWatermarks,
    };
    use tracedecay_temporal_query::resolution::ValidatedAuthorization;

    const NOW: UtcMicros = UtcMicros(1_000);

    struct FixedSnapshotAuthority {
        snapshot: TemporalExecutionSnapshot,
    }

    impl SymbolGraphCursorSnapshotAuthority for FixedSnapshotAuthority {
        fn snapshot(
            &self,
            _context: &RequestContext,
            _lane: &str,
            _observed_at: UtcMicros,
        ) -> Result<TemporalExecutionSnapshot, tracedecay_application::retrieval::PrimitiveFailure>
        {
            Ok(self.snapshot.clone())
        }
    }

    #[test]
    fn authenticated_cursor_rechecks_query_snapshot_bindings() {
        let (scope, context, _) = application_context("symbol-graph");
        let key = SignedCursorKeyRefV1 {
            key_id: SessionCursorKeyIdV1::new("cursor.pr12").expect("key id"),
            version: SessionCursorVersionV1::new(1).expect("key version"),
        };
        let authenticator =
            InMemoryCursorAuthenticator::new(key.clone(), vec![7_u8; 32]).expect("authenticator");
        let snapshots = FixedSnapshotAuthority {
            snapshot: cursor_snapshot(&scope, &context, key, 11),
        };
        let snapshots = Arc::new(snapshots);
        let authenticator = Arc::new(authenticator);
        let adapter = AuthenticatedSymbolGraphCursorAdapter::new(
            Arc::clone(&snapshots),
            Arc::clone(&authenticator),
        );

        let cursor = adapter
            .issue_cursor(&context, "search", 3, 8, NOW)
            .expect("issue cursor");
        assert_eq!(
            adapter
                .resume_offset(&context, "search", &cursor, NOW)
                .expect("resume cursor"),
            3
        );

        let changed_snapshots = FixedSnapshotAuthority {
            snapshot: cursor_snapshot(
                &scope,
                &context,
                SignedCursorKeyRefV1 {
                    key_id: SessionCursorKeyIdV1::new("cursor.pr12").expect("key id"),
                    version: SessionCursorVersionV1::new(1).expect("key version"),
                },
                12,
            ),
        };
        let changed = AuthenticatedSymbolGraphCursorAdapter::new(
            Arc::new(changed_snapshots),
            Arc::clone(&authenticator),
        );
        assert!(
            changed
                .resume_offset(&context, "search", &cursor, NOW)
                .is_err()
        );
        let (_, other_context, _) =
            application_context_for_project("symbol-graph", "project.pr12.other");
        assert!(
            adapter
                .resume_offset(&other_context, "search", &cursor, NOW)
                .is_err()
        );
    }

    fn cursor_snapshot(
        scope: &ResolvedScope,
        context: &RequestContext,
        key: SignedCursorKeyRefV1,
        watermark: u64,
    ) -> TemporalExecutionSnapshot {
        let request = TemporalSnapshotRequest::new(
            SessionId::new("session.pr12.symbol-graph").expect("session"),
            scope.scope_digest.as_str(),
            format!("sha256:{}", "d".repeat(64)),
            context.grant().digest.as_str(),
            TemporalModeV1::Current,
            RetrievalGrainV1::Occurrence,
        )
        .expect("snapshot request");
        TemporalExecutionSnapshot::new_authorized(
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
                    format!("sha256:{}", "c".repeat(64)),
                )
                .expect("configuration digest"),
            },
            Some(key),
            ValidatedAuthorization::Authorized,
        )
        .expect("execution snapshot")
    }

    fn application_context(suffix: &str) -> (ResolvedScope, RequestContext, ApplicationOperation) {
        application_context_for_project(suffix, "project.pr12")
    }

    fn application_context_for_project(
        suffix: &str,
        project_id: &str,
    ) -> (ResolvedScope, RequestContext, ApplicationOperation) {
        let scope = ResolvedScope::new(
            ProjectId::new(project_id).expect("project"),
            RepositoryId::new("repository.pr12").expect("repository"),
            WorktreeId::new("worktree.pr12").expect("worktree"),
            Some(RefId::new("refs/heads/pr12").expect("reference")),
        )
        .expect("scope");
        let capability =
            CapabilityId::new(format!("capability.pr12.{suffix}")).expect("capability");
        let use_case = UseCaseId::new(format!("use-case.pr12.{suffix}")).expect("use case");
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new(format!("grant.pr12.{suffix}")).expect("grant id"),
            1,
            ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("grant digest"),
            ActorId::new("actor.pr12.issuer").expect("issuer"),
            UtcMicros(1),
            UtcMicros(10_000),
            scope.clone(),
            BTreeSet::from([capability.clone()]),
            BTreeSet::from([use_case.clone()]),
            DisclosureClass::Evidence,
        )
        .expect("grant");
        let context = RequestContext::new(
            ActorId::new("actor.pr12.requester").expect("actor"),
            scope.clone(),
            grant,
            RequestId::new(format!("request.pr12.{suffix}")).expect("request id"),
            Deadline::new(UtcMicros(10_000)).expect("deadline"),
            CancellationContext::active(format!("cancel.pr12.{suffix}")).expect("cancellation"),
        )
        .expect("request context");
        let operation = ApplicationOperation::new(
            capability,
            use_case,
            ResultContractRef::new(
                SchemaId::new(format!("schema.pr12.{suffix}")).expect("schema"),
                1,
            )
            .expect("result contract"),
            true,
        );
        (scope, context, operation)
    }
}

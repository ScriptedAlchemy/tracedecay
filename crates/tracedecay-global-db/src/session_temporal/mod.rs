mod cursor_keys;
mod direct;
mod doctor_health;
pub mod execution;
mod expand;
mod hydration;
pub mod operations;
mod participant_freeze;
mod projection;
mod query;
mod rebuild;
mod refresh;
/// Released LCM response shaping over one canonical frozen-store snapshot. The
/// DB-free shaping it applies is owned by [`self::render`].
mod registered_lcm_render;
mod relation_projection;
mod relation_receipts;
pub use relation_projection::seed_session_relation_projection;
pub mod relations;
pub mod render;
mod retrieval;
mod schema;
mod sql;
pub mod store;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tracedecay_domain::{HydrationStateV1, RetrievalAnchorId, SessionId, SignedCursorKeyRefV1};
use tracedecay_graph_db::GraphNamespace;

use self::execution::{
    AuthorizedTaskSessionExecutionRequestV1, AuthorizedTemporalExecutionRequest,
    SessionTemporalExecutionError, SessionTemporalExecutionPort, SessionTemporalExecutionReport,
    TaskSessionExecutionOmissionReasonV1, TaskSessionExecutionOmissionV1,
    TaskSessionRankSelectorV1, TaskSessionReauthorizationStageV1,
    TaskSessionSelectionCallbackErrorV1, TaskSessionTemporalExecutionFutureV1,
    TaskSessionTemporalExecutionOutcomeV1, TaskSessionTemporalExecutionPortV1,
    TaskSessionTemporalExecutionReportV1, TemporalExecutionFuture,
};
use self::render::{CanonicalLcmSourceHydration, apply_canonical_summary_source_content};
use crate::{ProjectGraphRuntimePortV1, RegisteredGlobalDb};
use tracedecay_query::retrieval::evidence_lanes::{
    CanonicalTaskSessionCandidateExportPortV1, TaskSessionLaneRequestV1,
    TaskSessionLaneRetrieverV1, TaskSessionPlan23BindingV1,
};
use tracedecay_sessions::lcm::contracts::{
    LcmContentSlice, LcmDescribeRequest, LcmDescribeResponse, LcmDescribeTarget, LcmError,
    LcmExpandRequest, LcmExpandResponse, LcmExpandTarget, LcmSourceRef,
};
use tracedecay_sessions::runtime::git_correlation::{
    GitCorrelationError, GitEvidenceGraphRuntimePort, GitScopeFilter,
    git_evidence_projection_identity, recover_git_evidence_projection,
};
use tracedecay_store::SessionMessageRecord;
use tracedecay_temporal_query::context::VersionedTokenEstimator;
use tracedecay_temporal_query::cursor::{CursorError, StableSortKey, encode_cursor, verify_cursor};
use tracedecay_temporal_query::hydrate_temporal_candidate_selection;
use tracedecay_temporal_query::hydration::hydrate_selected;
use tracedecay_temporal_query::ports::{
    BindingDigest, ExecutionControl, KernelVersions, TemporalExecutionSnapshot,
};
use tracedecay_temporal_query::resolution::ValidatedAuthorization;
use tracedecay_temporal_query::{execute_temporal_candidate_export, execute_temporal_kernel};

pub use self::cursor_keys::GlobalDbCursorKeyProvider;
pub use self::direct::ResolvedDirectAnchor;
use self::hydration::GlobalDbTemporalHydrationPort;
use self::participant_freeze::freeze_participants;
use self::retrieval::GlobalDbTemporalReadPort;
use self::sql::TemporalSqlRead;
use tracedecay_sessions::runtime::lcm::payload::read_verified_payload_content_with_checkpoint;

pub use doctor_health::{
    SessionTemporalHealthFindingKind, SessionTemporalHealthReport, SessionTemporalHealthStatus,
};
pub use projection::record_canonical_observation_effect;
pub use refresh::{SessionRefreshRecoveryV1, SessionRefreshRestartStateV1};
pub(crate) use schema::{
    SessionTemporalSchemaAdmission, install_session_temporal_schema,
    require_admissible_session_temporal_schema,
};
pub use store::GlobalDbSessionTemporalStore;

/// Computes status from the canonical immutable-summary publication records.
///
/// The renderer owns the implementation; this crate-private boundary keeps
/// the status route available to the registered LCM facade without exposing
/// the renderer module or adding a public API surface.
pub(crate) async fn canonical_lcm_summary_dag_status(
    snapshot: &tracedecay_runtime_core::db::engine::ReadSnapshot,
    provider: &str,
    session_id: Option<&str>,
) -> Result<tracedecay_sessions::runtime::lcm::LcmDagStatus, LcmError> {
    registered_lcm_render::canonical_lcm_summary_dag_status(snapshot, provider, session_id).await
}

struct RegisteredGitEvidenceGraphRuntime<'a>(&'a dyn ProjectGraphRuntimePortV1);

impl GitEvidenceGraphRuntimePort for RegisteredGitEvidenceGraphRuntime<'_> {
    fn publish_verified_manifest(
        &self,
        manifest: &tracedecay_graph_db::GraphGenerationManifest,
        idempotency_key: tracedecay_graph_db::GraphIdempotencyKey,
        cancelled: Arc<AtomicBool>,
    ) -> Result<tracedecay_graph_db::VerifiedGraphSnapshot, tracedecay_graph_db::GraphDbError> {
        self.0
            .publish_verified_manifest(manifest, idempotency_key, cancelled)
    }

    fn verified_snapshot(
        &self,
        projection: &tracedecay_graph_db::GraphProjectionIdentity,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Option<tracedecay_graph_db::VerifiedGraphSnapshot>, tracedecay_graph_db::GraphDbError>
    {
        self.0.verified_snapshot(projection, cancelled)
    }
}

impl RegisteredGlobalDb {
    /// Resolves a Git filter through the verified Git-evidence graph.
    ///
    /// `None` means the request is unscoped. A non-empty filter always returns
    /// `Some`, including an authoritative empty set when the graph has no
    /// matching sessions.
    pub fn git_scope_session_ids(
        &self,
        filter: &GitScopeFilter,
    ) -> Result<Option<Vec<(String, String)>>, GitCorrelationError> {
        if filter.is_empty() {
            return Ok(None);
        }
        let runtime = self.project_graph_runtime().ok_or_else(|| {
            GitCorrelationError::Unavailable(
                "registered project graph runtime is not mounted".to_owned(),
            )
        })?;
        let identity = git_evidence_projection_identity(GraphNamespace::new("project")?)?;
        // Absence is not an authoritative empty projection. Until Git
        // evidence has been published, callers cannot prove that no durable
        // session holds a matching worktree.
        let Some(projection) = recover_git_evidence_projection(
            &RegisteredGitEvidenceGraphRuntime(runtime.as_ref()),
            &identity,
            Arc::new(AtomicBool::new(false)),
        )?
        else {
            return Err(GitCorrelationError::Unavailable(
                "verified Git-evidence projection has not been published".to_owned(),
            ));
        };
        let session_ids = projection.session_ids_for_scope(filter).ok_or_else(|| {
            GitCorrelationError::Contract(
                "Git scope resolution requires a non-empty filter".to_owned(),
            )
        })?;
        Ok(Some(session_ids))
    }

    pub async fn ensure_active_session_cursor_key_result(
        &self,
    ) -> tracedecay_store::SessionStoreResult<SignedCursorKeyRefV1> {
        const OPERATION: &str = "provision registered session cursor authentication key";
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| query::storage(OPERATION, error))?;
        let key =
            cursor_keys::ensure_active_session_cursor_key_in_transaction(&transaction).await?;
        transaction
            .commit()
            .await
            .map_err(|error| query::storage(OPERATION, error))?;
        Ok(key)
    }

    pub async fn load_session_cursor_key_provider_result(
        &self,
    ) -> Result<GlobalDbCursorKeyProvider, cursor_keys::GlobalDbCursorKeyProviderError> {
        let key = self
            .ensure_active_session_cursor_key_result()
            .await
            .map_err(|source| cursor_keys::GlobalDbCursorKeyProviderError::Provision { source })?;
        let read = self.read_snapshot().await.map_err(|source| {
            cursor_keys::GlobalDbCursorKeyProviderError::Storage {
                operation: "load registered session cursor authentication key",
                source,
            }
        })?;
        GlobalDbCursorKeyProvider::from_registered_key_ref(&read, key).await
    }

    pub async fn load_preprovisioned_session_cursor_key_provider_result(
        &self,
    ) -> Result<GlobalDbCursorKeyProvider, cursor_keys::GlobalDbCursorKeyProviderError> {
        let read = self.read_snapshot().await.map_err(|source| {
            cursor_keys::GlobalDbCursorKeyProviderError::Storage {
                operation: "load pre-provisioned session cursor authentication key",
                source,
            }
        })?;
        GlobalDbCursorKeyProvider::from_registered_active(&read).await
    }
}

/// Registry-backed rendering adapter over one session shard.
pub struct RegisteredGlobalDbSessionTemporalExecution<'db> {
    db: &'db RegisteredGlobalDb,
}

impl<'db> RegisteredGlobalDbSessionTemporalExecution<'db> {
    pub const fn new(db: &'db RegisteredGlobalDb) -> Self {
        Self { db }
    }

    pub async fn session_message_from_hydrated_occurrence(
        &self,
        snapshot: &TemporalExecutionSnapshot,
        anchor_id: &RetrievalAnchorId,
        provider: &str,
        session_id: &str,
        content: &[u8],
    ) -> Result<SessionMessageRecord, SessionTemporalExecutionError> {
        let read = self
            .db
            .read_snapshot()
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        hydration::session_message_from_hydrated_bytes(
            &TemporalSqlRead::registered(&read),
            snapshot,
            anchor_id,
            provider,
            session_id,
            content,
        )
        .await
        .map_err(|_| SessionTemporalExecutionError::Unavailable)
    }

    pub async fn resolve_lcm_describe_target(
        &self,
        provider: &str,
        session_id: &SessionId,
        target: &LcmDescribeTarget,
    ) -> Result<Option<ResolvedDirectAnchor>, SessionTemporalExecutionError> {
        let read = self
            .db
            .read_snapshot()
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        direct::resolve_describe_target(
            &TemporalSqlRead::registered(&read),
            provider,
            session_id,
            target,
        )
        .await
    }

    pub async fn resolve_lcm_expand_target(
        &self,
        provider: &str,
        session_id: &SessionId,
        target: &LcmExpandTarget,
    ) -> Result<ResolvedDirectAnchor, SessionTemporalExecutionError> {
        let read = self
            .db
            .read_snapshot()
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        direct::resolve_expand_target(
            &TemporalSqlRead::registered(&read),
            provider,
            session_id,
            target,
        )
        .await
    }

    pub async fn render_lcm_describe(
        &self,
        request: LcmDescribeRequest,
        control: Option<&ExecutionControl>,
    ) -> Result<LcmDescribeResponse, SessionTemporalExecutionError> {
        let snapshot = self
            .db
            .read_snapshot()
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        tracedecay_sessions::runtime::lcm::require_privacy_remediated(&snapshot)
            .await
            .map_err(map_lcm_error)?;
        if let Some(control) = control {
            control.checkpoint().map_err(map_control_error)?;
        }
        let response = registered_lcm_render::describe(&snapshot, request)
            .await
            .map_err(map_lcm_error)?;
        if let Some(control) = control {
            control.checkpoint().map_err(map_control_error)?;
        }
        Ok(response)
    }

    pub async fn render_lcm_expand(
        &self,
        request: LcmExpandRequest,
        canonical_content: &str,
        control: &ExecutionControl,
    ) -> Result<LcmExpandResponse, SessionTemporalExecutionError> {
        let snapshot = self
            .db
            .read_snapshot()
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        tracedecay_sessions::runtime::lcm::require_privacy_remediated(&snapshot)
            .await
            .map_err(map_lcm_error)?;
        control.checkpoint().map_err(map_control_error)?;
        let response = registered_lcm_render::expand(&snapshot, request, canonical_content)
            .await
            .map_err(map_lcm_error)?;
        control.checkpoint().map_err(map_control_error)?;
        Ok(response)
    }

    pub async fn hydrate_lcm_external_payload(
        &self,
        snapshot: &TemporalExecutionSnapshot,
        anchor_id: &RetrievalAnchorId,
        provider: &str,
        session_id: &SessionId,
        payload_ref: &str,
        max_bytes: usize,
    ) -> Result<String, SessionTemporalExecutionError> {
        let read_snapshot = self
            .db
            .read_snapshot()
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let resolution = hydration::resolve_external_target(
            &TemporalSqlRead::registered(&read_snapshot),
            snapshot,
            anchor_id,
            provider,
            session_id.as_str(),
            payload_ref,
        )
        .await
        .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let descriptor = match resolution {
            hydration::HydrationResolution::Available(descriptor) => descriptor,
            hydration::HydrationResolution::Unavailable(state) => {
                return Err(match state {
                    HydrationStateV1::Locked => SessionTemporalExecutionError::Locked,
                    HydrationStateV1::Redacted => SessionTemporalExecutionError::Redacted,
                    HydrationStateV1::Deleted | HydrationStateV1::RetentionExpired => {
                        SessionTemporalExecutionError::Deleted
                    }
                    HydrationStateV1::Unauthorized => SessionTemporalExecutionError::Denied,
                    HydrationStateV1::Available
                    | HydrationStateV1::RetainedButUnavailable
                    | HydrationStateV1::UnverifiableLegacy => {
                        SessionTemporalExecutionError::Unavailable
                    }
                });
            }
        };
        if descriptor.byte_count > max_bytes {
            return Err(SessionTemporalExecutionError::BudgetExhausted);
        }
        let hydration::PayloadSource::External {
            provider: descriptor_provider,
            session_id: descriptor_session,
            payload_ref: descriptor_ref,
            char_count,
        } = descriptor.source
        else {
            return Err(SessionTemporalExecutionError::Unavailable);
        };
        if descriptor_provider != provider
            || descriptor_session != session_id.as_str()
            || descriptor_ref != payload_ref
        {
            return Err(SessionTemporalExecutionError::Denied);
        }
        let storage_root = self
            .db
            .db_path()
            .parent()
            .ok_or(SessionTemporalExecutionError::Unavailable)?;
        let control = snapshot.request().execution_control();
        let mut checkpoint = || {
            control.checkpoint().map_err(|error| match error {
                tracedecay_temporal_query::ports::TemporalPortError::Cancelled => {
                    LcmError::Cancelled
                }
                tracedecay_temporal_query::ports::TemporalPortError::DeadlineExceeded => {
                    LcmError::DeadlineExceeded
                }
                tracedecay_temporal_query::ports::TemporalPortError::BudgetExceeded { .. } => {
                    LcmError::BudgetExhausted
                }
                _ => LcmError::Db("temporal verification control failed".to_string()),
            })
        };
        read_verified_payload_content_with_checkpoint(
            storage_root,
            &descriptor_ref,
            &descriptor.content_hash,
            descriptor.byte_count,
            char_count,
            &mut checkpoint,
        )
        .map_err(map_lcm_error)
    }

    pub async fn hydrate_lcm_summary_sources(
        &self,
        snapshot: &TemporalExecutionSnapshot,
        provider: &str,
        session_id: &SessionId,
        slice: LcmContentSlice,
        expansion: &mut LcmExpandResponse,
    ) -> Result<(), SessionTemporalExecutionError> {
        if expansion.summary_sources.is_empty() {
            return Ok(());
        }
        let read_snapshot = self
            .db
            .read_snapshot()
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let read = TemporalSqlRead::registered(&read_snapshot);
        let mut resolutions = Vec::with_capacity(expansion.summary_sources.len());
        let mut anchors = Vec::with_capacity(expansion.summary_sources.len());
        for source in &expansion.summary_sources {
            let target = match &source.source_ref {
                LcmSourceRef::RawMessage { store_id } => LcmExpandTarget::RawMessage {
                    store_id: *store_id,
                },
                LcmSourceRef::SummaryNode { node_id } => LcmExpandTarget::SummaryNode {
                    node_id: node_id.clone(),
                },
            };
            let resolution =
                direct::resolve_expand_target(&read, provider, session_id, &target).await;
            match resolution {
                Ok(resolved) if &resolved.owner_session_id == session_id => {
                    anchors.push(resolved.anchor_id.clone());
                    resolutions.push(Ok(resolved.anchor_id));
                }
                Ok(_)
                | Err(
                    SessionTemporalExecutionError::Denied
                    | SessionTemporalExecutionError::WrongScope,
                ) => {
                    resolutions.push(Err(HydrationStateV1::Unauthorized));
                }
                Err(SessionTemporalExecutionError::Deleted) => {
                    resolutions.push(Err(HydrationStateV1::Deleted));
                }
                Err(SessionTemporalExecutionError::Redacted) => {
                    resolutions.push(Err(HydrationStateV1::Redacted));
                }
                Err(SessionTemporalExecutionError::Locked) => {
                    resolutions.push(Err(HydrationStateV1::Locked));
                }
                Err(SessionTemporalExecutionError::BudgetExhausted) => {
                    return Err(SessionTemporalExecutionError::BudgetExhausted);
                }
                Err(SessionTemporalExecutionError::Cancelled) => {
                    return Err(SessionTemporalExecutionError::Cancelled);
                }
                Err(_) => {
                    resolutions.push(Err(HydrationStateV1::RetainedButUnavailable));
                }
            }
        }
        let storage_root = self
            .db
            .db_path()
            .parent()
            .ok_or(SessionTemporalExecutionError::Unavailable)?;
        let (relation_scope, relation_store) = self
            .db
            .session_relation_store()
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let authority = GlobalDbTemporalHydrationPort::for_registered_snapshot_with_relations(
            &read_snapshot,
            storage_root,
            relation_scope,
            relation_store,
        );
        let batch = hydrate_selected(&authority, snapshot, &anchors)
            .await
            .map_err(|error| {
                SessionTemporalExecutionError::Kernel(
                    tracedecay_temporal_query::TemporalKernelError::Hydration(error),
                )
            })?;
        let available = batch
            .available
            .iter()
            .filter_map(|payload| {
                String::from_utf8(payload.bytes().to_vec())
                    .ok()
                    .map(|content| (payload.anchor_id().clone(), content))
            })
            .collect::<BTreeMap<_, _>>();
        let unavailable = batch
            .unavailable
            .iter()
            .map(|denial| (denial.anchor_id().clone(), denial.state()))
            .collect::<BTreeMap<_, _>>();
        let hydration = expansion
            .summary_sources
            .iter()
            .zip(resolutions)
            .map(|(source, resolution)| {
                let (state, content) = match resolution {
                    Ok(anchor_id) => {
                        if let Some(content) = available.get(&anchor_id) {
                            (HydrationStateV1::Available, Some(content.clone()))
                        } else {
                            (
                                unavailable
                                    .get(&anchor_id)
                                    .copied()
                                    .unwrap_or(HydrationStateV1::RetainedButUnavailable),
                                None,
                            )
                        }
                    }
                    Err(state) => (state, None),
                };
                CanonicalLcmSourceHydration {
                    source_ref: source.source_ref.clone(),
                    state,
                    content,
                }
            })
            .collect::<Vec<_>>();
        apply_canonical_summary_source_content(expansion, slice, &hydration)
            .map_err(|_| SessionTemporalExecutionError::Unavailable)
    }

    pub async fn encode_lcm_source_cursor(
        &self,
        snapshot: &TemporalExecutionSnapshot,
        binding: &str,
        next_source_offset: usize,
    ) -> Result<String, SessionTemporalExecutionError> {
        let read = self
            .db
            .read_snapshot()
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let authenticator = GlobalDbCursorKeyProvider::from_registered_snapshot(&read, snapshot)
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        encode_cursor(
            snapshot,
            &lcm_source_cursor_sort_key(binding, next_source_offset),
            &authenticator,
        )
        .map_err(map_lcm_cursor_error)
    }

    pub async fn decode_lcm_source_cursor(
        &self,
        snapshot: &TemporalExecutionSnapshot,
        binding: &str,
        encoded: &str,
    ) -> Result<usize, SessionTemporalExecutionError> {
        let read = self
            .db
            .read_snapshot()
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let authenticator = GlobalDbCursorKeyProvider::from_registered_snapshot(&read, snapshot)
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let sort_key =
            verify_cursor(encoded, snapshot, &authenticator).map_err(map_lcm_cursor_error)?;
        parse_lcm_source_cursor_offset(binding, &sort_key)
    }

    async fn freeze(
        &self,
        request: &AuthorizedTemporalExecutionRequest,
    ) -> Result<
        (
            tracedecay_runtime_core::db::engine::ReadSnapshot,
            TemporalExecutionSnapshot,
        ),
        SessionTemporalExecutionError,
    > {
        let control = request.snapshot_request().execution_control();
        control.checkpoint().map_err(map_control_error)?;
        let read = self
            .db
            .read_snapshot()
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let (participants, watermarks, cursor_key) =
            freeze_participants(&TemporalSqlRead::registered(&read), request).await?;
        control.checkpoint().map_err(map_control_error)?;
        let snapshot = TemporalExecutionSnapshot::new_authorized(
            request.snapshot_request().clone(),
            watermarks,
            KernelVersions {
                schema: request.schema_version(),
                ranking: request.ranking_version(),
                configuration_digest: BindingDigest::new(
                    "configuration_digest",
                    request.configuration_digest(),
                )
                .map_err(|_| SessionTemporalExecutionError::Unavailable)?,
            },
            cursor_key,
            ValidatedAuthorization::Authorized,
        )
        .and_then(|snapshot| snapshot.with_participant_manifest(participants))
        .map_err(map_control_error)?;
        Ok((read, snapshot))
    }
}

impl SessionTemporalExecutionPort for RegisteredGlobalDbSessionTemporalExecution<'_> {
    fn execute<'a, E>(
        &'a self,
        request: AuthorizedTemporalExecutionRequest,
        estimator: &'a E,
    ) -> TemporalExecutionFuture<'a>
    where
        E: VersionedTokenEstimator + Sync + 'a,
    {
        Box::pin(async move {
            let (read_snapshot, snapshot) = self.freeze(&request).await?;
            let authenticator =
                GlobalDbCursorKeyProvider::from_registered_snapshot(&read_snapshot, &snapshot)
                    .await
                    .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
            let storage_root = self
                .db
                .db_path()
                .parent()
                .ok_or(SessionTemporalExecutionError::Unavailable)?;
            let (relation_scope, relation_store) = self
                .db
                .session_relation_store()
                .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
            let kernel_request = request.into_kernel_request(snapshot);
            let read = GlobalDbTemporalReadPort::new_registered_with_relations(
                &read_snapshot,
                relation_scope,
                relation_store.clone(),
            );
            let hydration = GlobalDbTemporalHydrationPort::for_registered_snapshot_with_relations(
                &read_snapshot,
                storage_root,
                relation_scope,
                relation_store,
            );
            let result = execute_temporal_kernel(
                &kernel_request,
                &read,
                &hydration,
                &authenticator,
                estimator,
            )
            .await
            .map_err(SessionTemporalExecutionError::Kernel)?;
            let source_coverage = result
                .snapshot
                .source_coverage()
                .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
            Ok(SessionTemporalExecutionReport::from_source_coverage(
                result,
                source_coverage,
            ))
        })
    }
}

impl TaskSessionTemporalExecutionPortV1 for RegisteredGlobalDbSessionTemporalExecution<'_> {
    fn execute_task_session<'a, E>(
        &'a self,
        request: AuthorizedTaskSessionExecutionRequestV1,
        selector: &'a dyn TaskSessionRankSelectorV1,
        estimator: &'a E,
    ) -> TaskSessionTemporalExecutionFutureV1<'a>
    where
        E: VersionedTokenEstimator + Sync + 'a,
    {
        Box::pin(async move {
            let (read_snapshot, snapshot) = self.freeze(request.temporal()).await?;
            let authenticator =
                GlobalDbCursorKeyProvider::from_registered_snapshot(&read_snapshot, &snapshot)
                    .await
                    .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
            let storage_root = self
                .db
                .db_path()
                .parent()
                .ok_or(SessionTemporalExecutionError::Unavailable)?;
            let (relation_scope, relation_store) = self
                .db
                .session_relation_store()
                .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
            let kernel_request = request.temporal().clone().into_kernel_request(snapshot);
            let read = GlobalDbTemporalReadPort::new_registered_with_relations(
                &read_snapshot,
                relation_scope,
                relation_store.clone(),
            );
            let hydration = GlobalDbTemporalHydrationPort::for_registered_snapshot_with_relations(
                &read_snapshot,
                storage_root,
                relation_scope,
                relation_store,
            );
            let export = execute_temporal_candidate_export(&kernel_request, &read, &authenticator)
                .await
                .map_err(SessionTemporalExecutionError::Kernel)?;
            let plan23 = TaskSessionPlan23BindingV1::from_export(&export)
                .map_err(|error| task_session_callback_contract(error.to_string()))?;
            let candidate_port = CanonicalTaskSessionCandidateExportPortV1::new(
                &export,
                request.retriever_revision().clone(),
                request.score_domain().clone(),
                request.policy_revision().clone(),
            );
            let lane_request = TaskSessionLaneRequestV1::new(
                request.retrieval(),
                request.query(),
                request.binding(),
                &plan23,
                request.control(),
            );
            let lane_outcome = TaskSessionLaneRetrieverV1::new(&candidate_port)
                .execute(&lane_request)
                .map_err(|error| task_session_callback_contract(error.to_string()))?;

            if let Some(omission) = task_session_reauthorize(
                selector,
                request.binding(),
                TaskSessionReauthorizationStageV1::BeforeSelection,
            )? {
                return Ok(TaskSessionTemporalExecutionOutcomeV1::Omitted(omission));
            }
            let selection = match selector.select(
                request.binding(),
                request.retrieval(),
                request.query(),
                &lane_outcome,
            ) {
                Ok(selection) => selection,
                Err(error) => {
                    if let Some(omission) = task_session_callback_omission(
                        TaskSessionReauthorizationStageV1::BeforeSelection,
                        error,
                    )? {
                        return Ok(TaskSessionTemporalExecutionOutcomeV1::Omitted(omission));
                    }
                    return Err(task_session_callback_contract(
                        "task/session selector returned no outcome".to_owned(),
                    ));
                }
            };
            if selection.selected_anchors().len()
                > request.retrieval().budget.max_hydrated_results as usize
            {
                return Err(SessionTemporalExecutionError::BudgetExhausted);
            }
            if let Some(omission) = task_session_reauthorize(
                selector,
                request.binding(),
                TaskSessionReauthorizationStageV1::BeforeHydration,
            )? {
                return Ok(TaskSessionTemporalExecutionOutcomeV1::Omitted(omission));
            }
            let result = hydrate_temporal_candidate_selection(
                &kernel_request,
                export,
                selection.selected_anchors(),
                &hydration,
                estimator,
            )
            .await
            .map_err(SessionTemporalExecutionError::Kernel)?;
            let source_coverage = result
                .snapshot
                .source_coverage()
                .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
            Ok(TaskSessionTemporalExecutionOutcomeV1::Complete(
                TaskSessionTemporalExecutionReportV1 {
                    binding: request.binding().clone(),
                    selection,
                    temporal: SessionTemporalExecutionReport::from_source_coverage(
                        result,
                        source_coverage,
                    ),
                },
            ))
        })
    }
}

fn task_session_reauthorize(
    selector: &dyn TaskSessionRankSelectorV1,
    binding: &tracedecay_query::retrieval::evidence_lanes::TaskSessionBindingV1,
    stage: TaskSessionReauthorizationStageV1,
) -> Result<Option<TaskSessionExecutionOmissionV1>, SessionTemporalExecutionError> {
    match selector.reauthorize(binding, stage) {
        Ok(()) => Ok(None),
        Err(error) => task_session_callback_omission(stage, error),
    }
}

fn task_session_callback_omission(
    stage: TaskSessionReauthorizationStageV1,
    error: TaskSessionSelectionCallbackErrorV1,
) -> Result<Option<TaskSessionExecutionOmissionV1>, SessionTemporalExecutionError> {
    let reason = match error {
        TaskSessionSelectionCallbackErrorV1::Denied => TaskSessionExecutionOmissionReasonV1::Denied,
        TaskSessionSelectionCallbackErrorV1::Stale => TaskSessionExecutionOmissionReasonV1::Stale,
        TaskSessionSelectionCallbackErrorV1::Unavailable => {
            TaskSessionExecutionOmissionReasonV1::Unavailable
        }
        TaskSessionSelectionCallbackErrorV1::Invalid(detail) => {
            return Err(task_session_callback_contract(detail));
        }
    };
    Ok(Some(TaskSessionExecutionOmissionV1 { stage, reason }))
}

fn task_session_callback_contract(detail: String) -> SessionTemporalExecutionError {
    SessionTemporalExecutionError::Kernel(
        tracedecay_temporal_query::TemporalKernelError::CandidateExportContract(detail),
    )
}

fn map_control_error(
    error: tracedecay_temporal_query::ports::TemporalPortError,
) -> SessionTemporalExecutionError {
    match error {
        tracedecay_temporal_query::ports::TemporalPortError::Cancelled
        | tracedecay_temporal_query::ports::TemporalPortError::DeadlineExceeded => {
            SessionTemporalExecutionError::Cancelled
        }
        tracedecay_temporal_query::ports::TemporalPortError::BudgetExceeded { .. } => {
            SessionTemporalExecutionError::BudgetExhausted
        }
        error @ (tracedecay_temporal_query::ports::TemporalPortError::ParticipantLimitExceeded {
            ..
        } | tracedecay_temporal_query::ports::TemporalPortError::ParticipantManifestBytesExceeded {
            ..
        }) => SessionTemporalExecutionError::Kernel(
            tracedecay_temporal_query::TemporalKernelError::Port(error),
        ),
        // The caller distinguishes a genuinely source-free root from sources
        // that exist but have not published a searchable generation.
        tracedecay_temporal_query::ports::TemporalPortError::EmptyParticipantManifest => {
            SessionTemporalExecutionError::Unavailable
        }
        _ => SessionTemporalExecutionError::Unavailable,
    }
}

fn map_lcm_error(error: LcmError) -> SessionTemporalExecutionError {
    match error {
        LcmError::SummaryNodeNotFound
        | LcmError::PayloadNotFound
        | LcmError::PayloadMissing
        | LcmError::PayloadGcd => SessionTemporalExecutionError::Deleted,
        LcmError::PayloadLocked => SessionTemporalExecutionError::Locked,
        LcmError::PayloadNotOwnedBySession | LcmError::SummarySourceNotOwnedBySession => {
            SessionTemporalExecutionError::Denied
        }
        LcmError::Cancelled | LcmError::DeadlineExceeded => {
            SessionTemporalExecutionError::Cancelled
        }
        LcmError::BudgetExhausted => SessionTemporalExecutionError::BudgetExhausted,
        _ => SessionTemporalExecutionError::Unavailable,
    }
}

fn lcm_source_cursor_sort_key(binding: &str, next_source_offset: usize) -> StableSortKey {
    StableSortKey {
        normalized_score_micros: 0,
        knowledge_at_micros: 0,
        stable_id: format!("lcm-source:{binding}:{next_source_offset}"),
    }
}

fn parse_lcm_source_cursor_offset(
    binding: &str,
    sort_key: &StableSortKey,
) -> Result<usize, SessionTemporalExecutionError> {
    if sort_key.normalized_score_micros != 0 || sort_key.knowledge_at_micros != 0 {
        return Err(SessionTemporalExecutionError::Denied);
    }
    let prefix = format!("lcm-source:{binding}:");
    let offset = sort_key
        .stable_id
        .strip_prefix(&prefix)
        .ok_or(SessionTemporalExecutionError::Denied)?;
    offset
        .parse()
        .map_err(|_| SessionTemporalExecutionError::Denied)
}

fn map_lcm_cursor_error(error: CursorError) -> SessionTemporalExecutionError {
    match error {
        CursorError::RootMismatch
        | CursorError::SessionMismatch
        | CursorError::WrongAccess
        | CursorError::TemporalModeMismatch
        | CursorError::GrainMismatch => SessionTemporalExecutionError::WrongScope,
        CursorError::Malformed
        | CursorError::Tampered
        | CursorError::WrongRequest
        | CursorError::FilterMismatch
        | CursorError::SortKeyMismatch => SessionTemporalExecutionError::Denied,
        CursorError::Expired
        | CursorError::UnknownOrExpiredKey
        | CursorError::SchemaMismatch
        | CursorError::RankingMismatch
        | CursorError::ConfigurationMismatch
        | CursorError::GenerationMismatch
        | CursorError::ParticipantManifestMismatch
        | CursorError::EpochMismatch
        | CursorError::SourceWatermarkMismatch
        | CursorError::ProjectionWatermarkMismatch
        | CursorError::IndexWatermarkMismatch
        | CursorError::SummaryWatermarkMismatch
        | CursorError::KeyIdMismatch
        | CursorError::KeyVersionMismatch
        | CursorError::KeyUnavailable
        | CursorError::InvalidKeyMaterial => SessionTemporalExecutionError::Unavailable,
    }
}

#[cfg(test)]
mod cursor_access_tests {
    use super::*;

    #[test]
    fn request_rebinding_is_denied_while_missing_key_authority_is_unavailable() {
        assert!(matches!(
            map_lcm_cursor_error(CursorError::WrongRequest),
            SessionTemporalExecutionError::Denied
        ));
        assert!(matches!(
            map_lcm_cursor_error(CursorError::KeyUnavailable),
            SessionTemporalExecutionError::Unavailable
        ));
    }

    #[test]
    fn rank_final_revocation_is_a_typed_hydration_omission() {
        let omission = task_session_callback_omission(
            TaskSessionReauthorizationStageV1::BeforeHydration,
            TaskSessionSelectionCallbackErrorV1::Denied,
        )
        .expect("typed callback outcome")
        .expect("revocation omission");
        assert_eq!(
            omission,
            TaskSessionExecutionOmissionV1 {
                stage: TaskSessionReauthorizationStageV1::BeforeHydration,
                reason: TaskSessionExecutionOmissionReasonV1::Denied,
            },
        );

        let unavailable = task_session_callback_omission(
            TaskSessionReauthorizationStageV1::BeforeHydration,
            TaskSessionSelectionCallbackErrorV1::Unavailable,
        )
        .expect("typed callback outcome")
        .expect("unavailable omission");
        assert_eq!(
            unavailable.reason,
            TaskSessionExecutionOmissionReasonV1::Unavailable,
        );

        let stale = task_session_callback_omission(
            TaskSessionReauthorizationStageV1::BeforeSelection,
            TaskSessionSelectionCallbackErrorV1::Stale,
        )
        .expect("typed callback outcome")
        .expect("stale graph omission");
        assert_eq!(
            stale,
            TaskSessionExecutionOmissionV1 {
                stage: TaskSessionReauthorizationStageV1::BeforeSelection,
                reason: TaskSessionExecutionOmissionReasonV1::Stale,
            },
        );
    }
}

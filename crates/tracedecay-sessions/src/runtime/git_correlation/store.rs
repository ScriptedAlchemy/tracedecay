//! Store ports joining project-session receipts to verified graph authority.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest as _, Sha256};
use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphEntity, GraphEntityId, GraphEntityRef, GraphGenerationId,
    GraphGenerationManifest, GraphGenerationRelation, GraphIdempotencyKey, GraphLabel,
    GraphNamespace, GraphProjectionId, GraphProjectionIdentity, GraphProjectionReadRequest,
    GraphProjectorRevision, GraphProperty, GraphPropertyName, GraphRelationId, GraphRelationKind,
    GraphWatermark, SourceGeneration, VerifiedGraphSnapshot,
};
use tracedecay_runtime_core::{
    db::engine::{Executor, QueryExecutor, ReadSnapshot},
    store_runtime::VerifiedGraphRuntimePortV1,
};
use tracedecay_store::FactReadControl;

use super::{
    CommitSessionRecord, CorrelationIndexHealth, GitCorrelationError, GitEvidenceProjectionV1,
    GitScopeFilter, SessionGitCorrelationHit, SessionGitSpan, SessionsForQuery,
    canonical_provider_map,
};

const GRAPH_READ_PAGE_ITEMS: usize = 10_000;
const GIT_EVIDENCE_PROJECTION: &str = "session-git-evidence";
const SESSION_LABEL: &str = "GitEvidenceSession";
const SPAN_LABEL: &str = "GitEvidenceSpan";
const COMMIT_LABEL: &str = "GitEvidenceCommit";
const PROJECTION_RECORD_PROPERTY: &str = "projection-record";
const SPAN_RECORD_PROPERTY: &str = "span-record";
const COMMIT_SHA_PROPERTY: &str = "commit-sha";
const COMMIT_RECORD_PROPERTY: &str = "commit-evidence-record";
const SESSION_SPAN_RELATION: &str = "SessionHasGitSpan";
const SESSION_COMMIT_RELATION: &str = "SessionHasGitCommitEvidence";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsSessionTimestamp {
    pub provider: String,
    pub session_id: String,
    pub timestamp: i64,
}

pub trait AnalyticsSessionTimestampSource {
    fn as_analytics_session_timestamp(&self) -> Option<AnalyticsSessionTimestamp>;
}

impl AnalyticsSessionTimestampSource for AnalyticsSessionTimestamp {
    fn as_analytics_session_timestamp(&self) -> Option<AnalyticsSessionTimestamp> {
        Some(self.clone())
    }
}

pub fn git_evidence_projection_identity(
    namespace: GraphNamespace,
) -> Result<GraphProjectionIdentity, GitCorrelationError> {
    Ok(GraphProjectionIdentity::new(
        namespace,
        GraphProjectionId::new(GIT_EVIDENCE_PROJECTION)?,
    ))
}

pub fn git_evidence_generation_id(
    projection: &GitEvidenceProjectionV1,
    projector_revision: &GraphProjectorRevision,
) -> Result<GraphGenerationId, GitCorrelationError> {
    let bytes = serde_json::to_vec(&(
        "tracedecay.session-git-evidence-generation.v1",
        projection,
        projector_revision,
    ))?;
    GraphGenerationId::new(format!(
        "session-git-evidence:{}",
        hex::encode(Sha256::digest(bytes))
    ))
    .map_err(Into::into)
}

pub fn build_git_evidence_manifest_checked(
    identity: GraphProjectionIdentity,
    projection: &GitEvidenceProjectionV1,
    projector_revision: &GraphProjectorRevision,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<GraphGenerationManifest, GitCorrelationError> {
    check()?;
    if identity.projection.as_str() != GIT_EVIDENCE_PROJECTION {
        return Err(GitCorrelationError::Contract(
            "Git evidence projection identity uses a foreign projector".to_owned(),
        ));
    }
    let generation = git_evidence_generation_id(projection, projector_revision)?;
    let providers = canonical_provider_map(projection.spans(), projection.commit_sessions())?;
    let mut entities = vec![projection_entity(projection.source_watermark())?];
    let mut relations = Vec::new();
    for (session_id, provider) in &providers {
        entities.push(session_entity(session_id, provider)?);
    }
    for span in projection.spans() {
        check()?;
        entities.push(span_entity(span)?);
        relations.push(session_span_relation(&identity, span)?);
    }
    let mut commits = BTreeSet::new();
    for record in projection.commit_sessions() {
        check()?;
        if commits.insert(record.commit_sha.clone()) {
            entities.push(commit_entity(&record.commit_sha)?);
        }
        relations.push(session_commit_relation(&identity, record)?);
    }
    GraphGenerationManifest::new_checked(
        identity,
        generation,
        SourceGeneration::new(projection.source_watermark())?,
        GraphWatermark::new(projection.source_watermark())?,
        Vec::new(),
        entities,
        relations,
        check,
    )
    .map_err(Into::into)
}

pub trait GitCorrelationWriteTxn: QueryExecutor + Executor + Sized + Send {
    fn commit(self) -> impl Future<Output = Result<(), GitCorrelationError>> + Send;
}

/// The already-open project sessions authority plus its bound graph runtime.
///
/// SQL methods exist only for session activity and bounded-history receipts.
pub trait GitCorrelationSessionStore: Sync {
    type WriteTxn<'txn>: GitCorrelationWriteTxn
    where
        Self: 'txn;

    fn require_project_sessions_authority(&self) -> Result<(), GitCorrelationError>;

    fn read_snapshot(
        &self,
    ) -> impl Future<Output = Result<ReadSnapshot, GitCorrelationError>> + Send;

    fn open_write_transaction(
        &self,
    ) -> impl Future<Output = Result<Self::WriteTxn<'_>, GitCorrelationError>> + Send;

    fn graph_runtime(&self) -> Result<&dyn VerifiedGraphRuntimePortV1, GitCorrelationError>;
}

/// Typed query view recovered from one verified graph generation.
pub struct GitEvidenceProjectionStore {
    snapshot: VerifiedGraphSnapshot,
    projection: GitEvidenceProjectionV1,
}

impl std::fmt::Debug for GitEvidenceProjectionStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GitEvidenceProjectionStore")
            .field("projection", self.snapshot.projection())
            .field("generation", self.snapshot.generation())
            .field("span_count", &self.projection.spans().len())
            .field("commit_count", &self.projection.commit_sessions().len())
            .finish_non_exhaustive()
    }
}

impl GitEvidenceProjectionStore {
    pub fn from_verified_snapshot(
        snapshot: VerifiedGraphSnapshot,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Self, GitCorrelationError> {
        let identity = snapshot.projection().clone();
        let projection_property = GraphPropertyName::new(PROJECTION_RECORD_PROPERTY)?;
        let span_property = GraphPropertyName::new(SPAN_RECORD_PROPERTY)?;
        let commit_property = GraphPropertyName::new(COMMIT_RECORD_PROPERTY)?;
        let mut after_entity = None;
        let mut after_relation = None;
        let mut entities_done = false;
        let mut relations_done = false;
        let mut source_watermark = None;
        let mut spans = Vec::new();
        let mut commit_sessions = Vec::new();

        while !entities_done || !relations_done {
            let page = snapshot.read_projection(GraphProjectionReadRequest {
                namespace: identity.namespace.clone(),
                projection: identity.projection.clone(),
                after_entity: after_entity.clone(),
                after_relation: after_relation.clone(),
                max_entities: if entities_done {
                    0
                } else {
                    GRAPH_READ_PAGE_ITEMS
                },
                max_relations: if relations_done {
                    0
                } else {
                    GRAPH_READ_PAGE_ITEMS
                },
                cancellation: Arc::clone(&cancellation),
            })?;
            for entity in page.entities {
                if let Some(GraphProperty::String(value)) =
                    entity.properties.get(&projection_property)
                    && source_watermark.replace(value.clone()).is_some()
                {
                    return Err(GitCorrelationError::Corrupt(
                        "verified Git evidence contains duplicate projection metadata".to_owned(),
                    ));
                }
                if let Some(GraphProperty::Bytes(bytes)) = entity.properties.get(&span_property) {
                    spans.push(serde_json::from_slice(bytes)?);
                }
            }
            for relation in page.relations {
                if let Some(GraphProperty::Bytes(bytes)) = relation.properties.get(&commit_property)
                {
                    commit_sessions.push(serde_json::from_slice(bytes)?);
                }
            }
            after_entity = page.next_entity;
            after_relation = page.next_relation;
            entities_done = after_entity.is_none();
            relations_done = after_relation.is_none();
        }
        let source_watermark = source_watermark.ok_or_else(|| {
            GitCorrelationError::Corrupt(
                "verified Git evidence is missing projection metadata".to_owned(),
            )
        })?;
        let projection = GitEvidenceProjectionV1::new(source_watermark, spans, commit_sessions)?;
        Ok(Self {
            snapshot,
            projection,
        })
    }

    pub fn verified_snapshot(&self) -> &VerifiedGraphSnapshot {
        &self.snapshot
    }

    pub fn projection(&self) -> &GitEvidenceProjectionV1 {
        &self.projection
    }

    pub fn sessions_for(&self, query: &SessionsForQuery) -> Vec<SessionGitCorrelationHit> {
        self.projection
            .sessions_for(query, super::CommitRelationFilter::Produced)
    }

    pub fn sessions_for_with_relation(
        &self,
        query: &SessionsForQuery,
        relation: super::CommitRelationFilter,
    ) -> Vec<SessionGitCorrelationHit> {
        self.projection.sessions_for(query, relation)
    }

    pub fn session_ids_for_scope(&self, filter: &GitScopeFilter) -> Option<Vec<(String, String)>> {
        self.projection.session_ids_for_scope(filter)
    }

    pub fn health(&self, backfill_watermark: Option<i64>) -> CorrelationIndexHealth {
        CorrelationIndexHealth {
            projection_available: true,
            generation: Some(self.snapshot.generation().as_str().to_owned()),
            source_watermark: Some(self.projection.source_watermark().to_owned()),
            span_count: u64::try_from(self.projection.spans().len()).unwrap_or(u64::MAX),
            commit_count: u64::try_from(self.projection.commit_sessions().len())
                .unwrap_or(u64::MAX),
            backfill_watermark,
        }
    }
}

pub fn publish_git_evidence_projection(
    runtime: &dyn VerifiedGraphRuntimePortV1,
    identity: GraphProjectionIdentity,
    projection: &GitEvidenceProjectionV1,
    projector_revision: &GraphProjectorRevision,
    idempotency_key: GraphIdempotencyKey,
    cancelled: Arc<AtomicBool>,
) -> Result<GitEvidenceProjectionStore, GitCorrelationError> {
    let cancellation: Arc<dyn GraphCancellation> =
        Arc::new(AtomicGraphCancellation(Arc::clone(&cancelled)));
    let check = || {
        if cancelled.load(Ordering::Acquire) {
            Err(GraphDbError::Cancelled)
        } else {
            Ok(())
        }
    };
    let manifest =
        build_git_evidence_manifest_checked(identity, projection, projector_revision, &check)?;
    let snapshot = runtime.publish_verified_manifest(&manifest, idempotency_key, cancelled)?;
    GitEvidenceProjectionStore::from_verified_snapshot(snapshot, cancellation)
}

/// Recovers the published Git evidence projection, answering `Ok(None)` when
/// the projection has never published a verified head — the typed empty start
/// of a project without any recorded Git evidence.
pub fn recover_git_evidence_projection(
    runtime: &dyn VerifiedGraphRuntimePortV1,
    identity: &GraphProjectionIdentity,
    cancelled: Arc<AtomicBool>,
) -> Result<Option<GitEvidenceProjectionStore>, GitCorrelationError> {
    let read_cancelled = Arc::clone(&cancelled);
    let Some(snapshot) = runtime.verified_snapshot(
        identity,
        FactReadControl::new(Arc::new(move || read_cancelled.load(Ordering::Acquire))),
    )?
    else {
        return Ok(None);
    };
    GitEvidenceProjectionStore::from_verified_snapshot(
        snapshot,
        Arc::new(AtomicGraphCancellation(cancelled)),
    )
    .map(Some)
}

fn projection_entity(source_watermark: &str) -> Result<GraphEntity, GitCorrelationError> {
    GraphEntity::new(
        GraphEntityId::new("projection:session-git-evidence")?,
        BTreeSet::new(),
        BTreeMap::from([(
            GraphPropertyName::new(PROJECTION_RECORD_PROPERTY)?,
            GraphProperty::String(source_watermark.to_owned()),
        )]),
    )
    .map_err(Into::into)
}

fn session_entity(session_id: &str, provider: &str) -> Result<GraphEntity, GitCorrelationError> {
    GraphEntity::new(
        session_entity_id(session_id)?,
        BTreeSet::from([GraphLabel::new(SESSION_LABEL)?]),
        BTreeMap::from([(
            GraphPropertyName::new("provider")?,
            GraphProperty::String(provider.to_owned()),
        )]),
    )
    .map_err(Into::into)
}

fn span_entity(span: &SessionGitSpan) -> Result<GraphEntity, GitCorrelationError> {
    GraphEntity::new(
        span_entity_id(&span.span_id)?,
        BTreeSet::from([GraphLabel::new(SPAN_LABEL)?]),
        BTreeMap::from([(
            GraphPropertyName::new(SPAN_RECORD_PROPERTY)?,
            GraphProperty::Bytes(serde_json::to_vec(span)?),
        )]),
    )
    .map_err(Into::into)
}

fn commit_entity(commit_sha: &str) -> Result<GraphEntity, GitCorrelationError> {
    GraphEntity::new(
        commit_entity_id(commit_sha)?,
        BTreeSet::from([GraphLabel::new(COMMIT_LABEL)?]),
        BTreeMap::from([(
            GraphPropertyName::new(COMMIT_SHA_PROPERTY)?,
            GraphProperty::String(commit_sha.to_owned()),
        )]),
    )
    .map_err(Into::into)
}

fn session_span_relation(
    projection: &GraphProjectionIdentity,
    span: &SessionGitSpan,
) -> Result<GraphGenerationRelation, GitCorrelationError> {
    GraphGenerationRelation::new(
        GraphRelationId::new(stable_identity("session-span", &span.span_id))?,
        GraphEntityRef::new(projection.clone(), session_entity_id(&span.session_id)?),
        GraphEntityRef::new(projection.clone(), span_entity_id(&span.span_id)?),
        GraphRelationKind::new(SESSION_SPAN_RELATION)?,
        BTreeMap::new(),
    )
    .map_err(Into::into)
}

fn session_commit_relation(
    projection: &GraphProjectionIdentity,
    record: &CommitSessionRecord,
) -> Result<GraphGenerationRelation, GitCorrelationError> {
    GraphGenerationRelation::new(
        GraphRelationId::new(stable_identity(
            "session-commit",
            &format!("{}\0{}", record.session_id, record.commit_sha),
        ))?,
        GraphEntityRef::new(projection.clone(), session_entity_id(&record.session_id)?),
        GraphEntityRef::new(projection.clone(), commit_entity_id(&record.commit_sha)?),
        GraphRelationKind::new(SESSION_COMMIT_RELATION)?,
        BTreeMap::from([(
            GraphPropertyName::new(COMMIT_RECORD_PROPERTY)?,
            GraphProperty::Bytes(serde_json::to_vec(record)?),
        )]),
    )
    .map_err(Into::into)
}

fn session_entity_id(session_id: &str) -> Result<GraphEntityId, GitCorrelationError> {
    GraphEntityId::new(stable_identity("session", session_id)).map_err(Into::into)
}

fn span_entity_id(span_id: &str) -> Result<GraphEntityId, GitCorrelationError> {
    GraphEntityId::new(stable_identity("span", span_id)).map_err(Into::into)
}

fn commit_entity_id(commit_sha: &str) -> Result<GraphEntityId, GitCorrelationError> {
    GraphEntityId::new(stable_identity("commit", commit_sha)).map_err(Into::into)
}

fn stable_identity(kind: &str, material: &str) -> String {
    format!(
        "{kind}:{}",
        hex::encode(Sha256::digest(material.as_bytes()))
    )
}

struct AtomicGraphCancellation(Arc<AtomicBool>);

impl GraphCancellation for AtomicGraphCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

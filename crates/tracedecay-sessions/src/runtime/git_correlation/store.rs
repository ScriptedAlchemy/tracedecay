//! Store ports joining project-session receipts to verified graph authority.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use sha2::{Digest as _, Sha256};
use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphEntity, GraphEntityId, GraphEntityRef, GraphGenerationId,
    GraphGenerationManifest, GraphGenerationRelation, GraphIdempotencyKey, GraphLabel,
    GraphNamespace, GraphProjectionId, GraphProjectionIdentity, GraphProjectionReadRequest,
    GraphProjectorRevision, GraphProperty, GraphPropertyName, GraphRelationId, GraphRelationKind,
    GraphRelationRef, GraphWatermark, SourceGeneration, VerifiedGraphSnapshot,
};
use tracedecay_runtime_core::{
    db::engine::{Executor, QueryExecutor},
    shard_runtime::VerifiedGraphRuntimePortV1,
};
use tracedecay_store::FactReadControl;

use super::{
    CommitRelationFilter, CommitSessionRecord, CorrelationIndexHealth, CorrelationIndexPresence,
    GIT_EVIDENCE_LEGACY_PROJECTOR_REVISION_V1, GIT_EVIDENCE_PROJECTOR_REVISION,
    GitCorrelationError, GitEvidenceProjectionV1, GitRefFilter, GitScopeFilter,
    SessionGitCorrelationHit, SessionGitSpan, SessionsForQuery, SpanObservation,
    canonical_provider_map, commit_hits, commit_identities_with_producer_fallback,
    commit_record_matches_query, commit_record_order, scope_session_ids, sessions_for_limit,
    span_hits, span_matches_query,
};

const GRAPH_READ_PAGE_ITEMS: usize = 10_000;
/// Relation-identity page width for hub fan-out reads. The graph serves pages
/// from one ordered adjacency index per hub, so a wider page only trades
/// per-call overhead against identities cloned past the query's stop point.
const HUB_FANOUT_PAGE_ITEMS: usize = 256;
const GIT_EVIDENCE_NAMESPACE: &str = "project";
const GIT_EVIDENCE_PROJECTION: &str = "session-git-evidence";
const SESSION_LABEL: &str = "GitEvidenceSession";
const SPAN_LABEL: &str = "GitEvidenceSpan";
const COMMIT_LABEL: &str = "GitEvidenceCommit";
const BRANCH_LABEL: &str = "GitEvidenceBranch";
const WORKTREE_LABEL: &str = "GitEvidenceWorktree";
const COMMIT_PREFIX_LABEL: &str = "GitEvidenceCommitPrefix";
const PROJECTION_RECORD_PROPERTY: &str = "projection-record";
const PROJECTOR_REVISION_PROPERTY: &str = "projector-revision";
const SPAN_COUNT_PROPERTY: &str = "span-count";
const COMMIT_COUNT_PROPERTY: &str = "commit-count";
const SESSION_ID_PROPERTY: &str = "session-id";
const PROVIDER_PROPERTY: &str = "provider";
const SPAN_RECORD_PROPERTY: &str = "span-record";
const COMMIT_SHA_PROPERTY: &str = "commit-sha";
const COMMIT_RECORD_PROPERTY: &str = "commit-evidence-record";
const BRANCH_PROPERTY: &str = "branch";
const WORKTREE_PROPERTY: &str = "worktree";
const COMMIT_PREFIX_PROPERTY: &str = "commit-prefix";
const SESSION_SPAN_RELATION: &str = "SessionHasGitSpan";
const SESSION_COMMIT_RELATION: &str = "SessionHasGitCommitEvidence";
const BRANCH_SPAN_RELATION: &str = "GitBranchHasSpan";
const WORKTREE_SPAN_RELATION: &str = "GitWorktreeHasSpan";
const COMMIT_PREFIX_COMMIT_RELATION: &str = "GitCommitPrefixHasCommit";
const BRANCH_SPAN_INDEX_KIND: &str = "branch-span";
const WORKTREE_SPAN_INDEX_KIND: &str = "worktree-span";
/// Commit lookups bucket every commit under its first six hex digits — the
/// shortest prefix [`super::GitRefFilter::parse`] admits — so any admitted
/// prefix resolves to exactly one bucket whose fan-out is the commits sharing
/// those digits, not the store.
const COMMIT_PREFIX_LEN: usize = 6;

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

/// Projects the complete evidence into one generation manifest.
///
/// Besides the session, span, and commit rows the manifest carries the query
/// index bounded reads depend on: a branch and a worktree hub per distinct
/// value whose relations to spans are keyed newest-activity-first
/// ([`SpanIndexKey`]), a commit-prefix hub per six-digit SHA prefix, and
/// projection metadata (projector revision, span and commit counts) so
/// health and presence never enumerate rows.
pub fn build_git_evidence_manifest_checked(
    identity: GraphProjectionIdentity,
    projection: &GitEvidenceProjectionV1,
    projector_revision: &GraphProjectorRevision,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<GraphGenerationManifest, GitCorrelationError> {
    check()?;
    if identity.namespace.as_str() != GIT_EVIDENCE_NAMESPACE
        || identity.projection.as_str() != GIT_EVIDENCE_PROJECTION
    {
        return Err(GitCorrelationError::Contract(
            "Git evidence projection identity uses a foreign namespace or projector".to_owned(),
        ));
    }
    if projector_revision.as_str() != GIT_EVIDENCE_PROJECTOR_REVISION {
        return Err(GitCorrelationError::Contract(format!(
            "Git evidence manifests are projected under `{GIT_EVIDENCE_PROJECTOR_REVISION}`, not `{}`",
            projector_revision.as_str()
        )));
    }
    let generation = git_evidence_generation_id(projection, projector_revision)?;
    let providers = canonical_provider_map(projection.spans(), projection.commit_sessions())?;
    let mut entities = vec![projection_entity(projection, projector_revision)?];
    let mut relations = Vec::new();
    for (session_id, provider) in &providers {
        entities.push(session_entity(session_id, provider)?);
    }
    let mut branches = BTreeSet::new();
    let mut worktrees = BTreeSet::new();
    for span in projection.spans() {
        check()?;
        entities.push(span_entity(span)?);
        relations.push(session_span_relation(&identity, span)?);
        if let Some(branch) = &span.branch {
            if branches.insert(branch.as_str()) {
                entities.push(branch_entity(branch)?);
            }
            relations.push(span_index_relation(
                &identity,
                branch_entity_id(branch)?,
                BRANCH_SPAN_RELATION,
                BRANCH_SPAN_INDEX_KIND,
                span,
            )?);
        }
        if worktrees.insert(span.worktree.as_str()) {
            entities.push(worktree_entity(&span.worktree)?);
        }
        relations.push(span_index_relation(
            &identity,
            worktree_entity_id(&span.worktree)?,
            WORKTREE_SPAN_RELATION,
            WORKTREE_SPAN_INDEX_KIND,
            span,
        )?);
    }
    let mut commits = BTreeSet::new();
    let mut prefixes = BTreeSet::new();
    for record in projection.commit_sessions() {
        check()?;
        if commits.insert(record.commit_sha.clone()) {
            entities.push(commit_entity(&record.commit_sha)?);
            let prefix = commit_prefix(&record.commit_sha)?;
            if prefixes.insert(prefix) {
                entities.push(commit_prefix_entity(prefix)?);
            }
            relations.push(commit_prefix_relation(
                &identity,
                prefix,
                &record.commit_sha,
            )?);
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

/// The exact pre-index (`v1`) generation shape for `projection`: no projector
/// revision marker, no counts, no hubs or index relations, and the legacy
/// generation identity. Lets dependent crates exercise their legacy-head
/// handling against the shape a live store published before this projector.
#[cfg(any(test, feature = "test-helpers"))]
pub fn legacy_git_evidence_manifest_for_test(
    identity: GraphProjectionIdentity,
    projection: &GitEvidenceProjectionV1,
) -> Result<GraphGenerationManifest, GitCorrelationError> {
    let legacy_revision =
        GraphProjectorRevision::try_from(GIT_EVIDENCE_LEGACY_PROJECTOR_REVISION_V1.to_owned())?;
    let generation = git_evidence_generation_id(projection, &legacy_revision)?;
    let providers = canonical_provider_map(projection.spans(), projection.commit_sessions())?;
    let mut entities = vec![GraphEntity::new(
        projection_entity_id()?,
        BTreeSet::new(),
        BTreeMap::from([(
            GraphPropertyName::new(PROJECTION_RECORD_PROPERTY)?,
            GraphProperty::String(projection.source_watermark().to_owned()),
        )]),
    )?];
    let mut relations = Vec::new();
    for (session_id, provider) in &providers {
        entities.push(GraphEntity::new(
            session_entity_id(session_id)?,
            BTreeSet::from([GraphLabel::new(SESSION_LABEL)?]),
            BTreeMap::from([(
                GraphPropertyName::new(PROVIDER_PROPERTY)?,
                GraphProperty::String(provider.to_owned()),
            )]),
        )?);
    }
    for span in projection.spans() {
        entities.push(span_entity(span)?);
        relations.push(session_span_relation(&identity, span)?);
    }
    let mut commits = BTreeSet::new();
    for record in projection.commit_sessions() {
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
        &|| Ok(()),
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
    /// A read view whose lifetime retains the exact client authority that
    /// issued it. Production stores use a guarded database-engine snapshot;
    /// standalone engine snapshots are confined to test stores.
    type ReadSnapshot: QueryExecutor + Send + Sync;

    type WriteTxn<'txn>: GitCorrelationWriteTxn
    where
        Self: 'txn;

    fn require_project_sessions_authority(&self) -> Result<(), GitCorrelationError>;

    fn read_snapshot(
        &self,
    ) -> impl Future<Output = Result<Self::ReadSnapshot, GitCorrelationError>> + Send;

    fn open_write_transaction(
        &self,
    ) -> impl Future<Output = Result<Self::WriteTxn<'_>, GitCorrelationError>> + Send;

    /// Publishes owned Git evidence without requiring async callers to retain
    /// borrowed buffers across the publication boundary.
    ///
    /// Test and standalone stores keep the synchronous implementation inline.
    /// Production registered stores override this to move the complete graph
    /// operation off the async runtime worker.
    fn publish_graph_evidence_owned(
        &self,
        publication_prefix: String,
        new_spans: Vec<SessionGitSpan>,
        new_commits: Vec<CommitSessionRecord>,
    ) -> impl Future<Output = Result<(usize, usize), GitCorrelationError>> + Send
    where
        Self: Sized,
    {
        async move {
            super::attribution::publish_graph_evidence(
                self,
                &publication_prefix,
                &new_spans,
                &new_commits,
            )
        }
    }

    /// Shapes and publishes owned transcript observations through the same
    /// production operation boundary as already-shaped Git evidence.
    fn publish_transcript_graph_evidence_owned(
        &self,
        publication_prefix: String,
        observations: Vec<SpanObservation>,
        new_commits: Vec<CommitSessionRecord>,
        merge_gap_secs: i64,
    ) -> impl Future<Output = Result<(usize, usize), GitCorrelationError>> + Send
    where
        Self: Sized,
    {
        async move {
            super::attribution::publish_transcript_graph_evidence(
                self,
                &publication_prefix,
                &observations,
                &new_commits,
                merge_gap_secs,
            )
        }
    }

    /// Serializes recovery, merge, and publication for this exact retained
    /// Git-evidence projection. The graph runtime's own publication gate
    /// starts after the caller has recovered its base generation, so it cannot
    /// by itself prevent two callers from replacing one another with sibling
    /// generations derived from the same head.
    fn git_evidence_publication_lock(&self) -> Result<Arc<Mutex<()>>, GitCorrelationError>;

    fn graph_runtime(&self) -> Result<&dyn VerifiedGraphRuntimePortV1, GitCorrelationError>;
}

/// Which projector revision published a verified Git-evidence head.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitEvidenceProjectorRevision {
    /// [`GIT_EVIDENCE_PROJECTOR_REVISION`]: carries the bounded query index.
    Current,
    /// [`GIT_EVIDENCE_LEGACY_PROJECTOR_REVISION_V1`]: rows only, no index.
    LegacyV1,
}

impl GitEvidenceProjectorRevision {
    /// Resolves the revision a head declares. Heads published before the
    /// projector recorded its revision are the legacy shape; any recorded
    /// revision other than the current one is a projector this build cannot
    /// serve.
    fn from_recorded(recorded: Option<&str>) -> Result<Self, GitCorrelationError> {
        match recorded {
            None => Ok(Self::LegacyV1),
            Some(GIT_EVIDENCE_PROJECTOR_REVISION) => Ok(Self::Current),
            Some(other) => Err(GitCorrelationError::Corrupt(format!(
                "verified Git evidence records unknown projector revision `{other}`"
            ))),
        }
    }

    fn graph_revision(self) -> Result<GraphProjectorRevision, GitCorrelationError> {
        let revision = match self {
            Self::Current => GIT_EVIDENCE_PROJECTOR_REVISION,
            Self::LegacyV1 => GIT_EVIDENCE_LEGACY_PROJECTOR_REVISION_V1,
        };
        GraphProjectorRevision::try_from(revision.to_owned()).map_err(Into::into)
    }
}

/// Complete typed projection recovered from one verified graph generation.
///
/// This is the publication and full-export view: recovery walks every row and
/// re-derives the generation identity from the decoded content. Bounded
/// production queries use [`GitEvidenceGraphView`] instead.
pub struct GitEvidenceProjectionStore {
    snapshot: VerifiedGraphSnapshot,
    projection: GitEvidenceProjectionV1,
    projector_revision: GitEvidenceProjectorRevision,
}

impl std::fmt::Debug for GitEvidenceProjectionStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GitEvidenceProjectionStore")
            .field("projection", self.snapshot.projection())
            .field("generation", self.snapshot.generation())
            .field("projector_revision", &self.projector_revision)
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
        require_git_evidence_projection_identity(&identity)?;
        let projection_property = GraphPropertyName::new(PROJECTION_RECORD_PROPERTY)?;
        let revision_property = GraphPropertyName::new(PROJECTOR_REVISION_PROPERTY)?;
        let span_property = GraphPropertyName::new(SPAN_RECORD_PROPERTY)?;
        let commit_property = GraphPropertyName::new(COMMIT_RECORD_PROPERTY)?;
        let mut after_entity = None;
        let mut after_relation = None;
        let mut entities_done = false;
        let mut relations_done = false;
        let mut source_watermark = None;
        let mut projector_revision = None;
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
                {
                    if source_watermark.replace(value.clone()).is_some() {
                        return Err(GitCorrelationError::Corrupt(
                            "verified Git evidence contains duplicate projection metadata"
                                .to_owned(),
                        ));
                    }
                    projector_revision = GitEvidenceProjectorRevision::from_recorded(match entity
                        .properties
                        .get(&revision_property)
                    {
                        Some(GraphProperty::String(recorded)) => Some(recorded.as_str()),
                        _ => None,
                    })
                    .map(Some)?;
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
        let (Some(source_watermark), Some(projector_revision)) =
            (source_watermark, projector_revision)
        else {
            return Err(GitCorrelationError::Corrupt(
                "verified Git evidence is missing projection metadata".to_owned(),
            ));
        };
        let projection = GitEvidenceProjectionV1::new(source_watermark, spans, commit_sessions)?;
        require_git_evidence_generation(
            &snapshot,
            &projection,
            &projector_revision.graph_revision()?,
        )?;
        Ok(Self {
            snapshot,
            projection,
            projector_revision,
        })
    }

    pub fn verified_snapshot(&self) -> &VerifiedGraphSnapshot {
        &self.snapshot
    }

    pub fn projection(&self) -> &GitEvidenceProjectionV1 {
        &self.projection
    }

    pub fn projector_revision(&self) -> GitEvidenceProjectorRevision {
        self.projector_revision
    }

    pub fn sessions_for(&self, query: &SessionsForQuery) -> Vec<SessionGitCorrelationHit> {
        self.projection
            .sessions_for(query, CommitRelationFilter::Produced)
    }

    pub fn sessions_for_with_relation(
        &self,
        query: &SessionsForQuery,
        relation: CommitRelationFilter,
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

    pub fn presence(&self, backfill_watermark: Option<i64>) -> CorrelationIndexPresence {
        CorrelationIndexPresence {
            projection_available: true,
            generation: Some(self.snapshot.generation().as_str().to_owned()),
            source_watermark: Some(self.projection.source_watermark().to_owned()),
            spans_present: !self.projection.spans().is_empty(),
            commits_present: !self.projection.commit_sessions().is_empty(),
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
    require_git_evidence_generation(&snapshot, projection, projector_revision)?;
    // Publication's verified-head CAS is the irreversible commit point. The
    // caller-supplied projection is the exact canonical manifest input, so do
    // not re-read the committed snapshot under a request cancellation token
    // and risk reporting `Cancelled` after durable success.
    Ok(GitEvidenceProjectionStore {
        snapshot,
        projection: projection.clone(),
        projector_revision: GitEvidenceProjectorRevision::Current,
    })
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

/// The project's Git-evidence head as bounded readers see it.
#[derive(Debug)]
pub enum GitEvidenceGraphHead {
    /// No verified head has ever been published: the typed empty start.
    Unpublished,
    /// The head was published before the projector carried a query index. Its
    /// rows are recoverable in full, but no bounded read can be served until
    /// the next publication re-projects it.
    Legacy {
        generation: GraphGenerationId,
    },
    Indexed(GitEvidenceGraphView),
}

/// Bounded query view over one verified Git-evidence generation.
///
/// Every read is served by point reads and hub fan-out against the persisted
/// rows, so the rows touched scale with the answer rather than the store.
/// Health and presence come from the projection entity's authenticated
/// metadata; session queries page a branch or worktree hub's
/// newest-first span index and hydrate only the sessions that can appear in
/// the result; commit queries resolve one six-digit prefix bucket. The view
/// retains the verified snapshot lease for its lifetime, so a publication
/// racing a read cannot mix two generations.
pub struct GitEvidenceGraphView {
    snapshot: VerifiedGraphSnapshot,
    cancellation: Arc<dyn GraphCancellation>,
    source_watermark: String,
    span_count: u64,
    commit_count: u64,
}

impl std::fmt::Debug for GitEvidenceGraphView {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GitEvidenceGraphView")
            .field("projection", self.snapshot.projection())
            .field("generation", self.snapshot.generation())
            .field("span_count", &self.span_count)
            .field("commit_count", &self.commit_count)
            .finish_non_exhaustive()
    }
}

/// Opens the bounded view over the published head without decoding any span
/// or commit payload: one point read of the projection entity establishes the
/// projector revision and the row-family counts.
#[hotpath::measure(label = "sessions.git_correlation.graph_view.open")]
pub fn open_git_evidence_graph_view(
    runtime: &dyn VerifiedGraphRuntimePortV1,
    identity: &GraphProjectionIdentity,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<GitEvidenceGraphHead, GitCorrelationError> {
    let read_cancellation = Arc::clone(&cancellation);
    let Some(snapshot) = runtime.verified_snapshot(
        identity,
        FactReadControl::new(Arc::new(move || read_cancellation.is_cancelled())),
    )?
    else {
        return Ok(GitEvidenceGraphHead::Unpublished);
    };
    require_git_evidence_projection_identity(snapshot.projection())?;
    let metadata = snapshot
        .entity(
            &GraphEntityRef::new(snapshot.projection().clone(), projection_entity_id()?),
            Arc::clone(&cancellation),
        )?
        .ok_or_else(|| {
            GitCorrelationError::Corrupt(
                "verified Git evidence is missing projection metadata".to_owned(),
            )
        })?;
    let revision = GitEvidenceProjectorRevision::from_recorded(string_property(
        &metadata.properties,
        PROJECTOR_REVISION_PROPERTY,
    )?)?;
    if revision == GitEvidenceProjectorRevision::LegacyV1 {
        return Ok(GitEvidenceGraphHead::Legacy {
            generation: snapshot.generation().clone(),
        });
    }
    let source_watermark = required_string_property(
        &metadata.properties,
        PROJECTION_RECORD_PROPERTY,
        "projection watermark",
    )?
    .to_owned();
    let span_count = required_count_property(&metadata.properties, SPAN_COUNT_PROPERTY)?;
    let commit_count = required_count_property(&metadata.properties, COMMIT_COUNT_PROPERTY)?;
    Ok(GitEvidenceGraphHead::Indexed(GitEvidenceGraphView {
        snapshot,
        cancellation,
        source_watermark,
        span_count,
        commit_count,
    }))
}

impl GitEvidenceGraphView {
    pub fn verified_snapshot(&self) -> &VerifiedGraphSnapshot {
        &self.snapshot
    }

    pub fn health(&self, backfill_watermark: Option<i64>) -> CorrelationIndexHealth {
        CorrelationIndexHealth {
            projection_available: true,
            generation: Some(self.snapshot.generation().as_str().to_owned()),
            source_watermark: Some(self.source_watermark.clone()),
            span_count: self.span_count,
            commit_count: self.commit_count,
            backfill_watermark,
        }
    }

    pub fn presence(&self, backfill_watermark: Option<i64>) -> CorrelationIndexPresence {
        CorrelationIndexPresence {
            projection_available: true,
            generation: Some(self.snapshot.generation().as_str().to_owned()),
            source_watermark: Some(self.source_watermark.clone()),
            spans_present: self.span_count > 0,
            commits_present: self.commit_count > 0,
            backfill_watermark,
        }
    }

    /// Same result set and ordering as
    /// [`GitEvidenceProjectionV1::sessions_for`] over the complete projection.
    #[hotpath::measure(label = "sessions.git_correlation.graph_view.sessions_for")]
    pub fn sessions_for(
        &self,
        query: &SessionsForQuery,
        relation: CommitRelationFilter,
    ) -> Result<Vec<SessionGitCorrelationHit>, GitCorrelationError> {
        let limit = sessions_for_limit(query);
        match &query.git_ref {
            GitRefFilter::Branch(branch) => {
                self.span_hits_from_hub(branch_entity_id(branch)?, BRANCH_SPAN_RELATION, query)
            }
            GitRefFilter::Worktree(worktree) => self.span_hits_from_hub(
                worktree_entity_id(worktree)?,
                WORKTREE_SPAN_RELATION,
                query,
            ),
            GitRefFilter::Commit(sha) => {
                let records = self.commit_records_with_prefix(sha)?;
                Ok(commit_hits(
                    records
                        .iter()
                        .filter(|record| commit_record_matches_query(record, sha, relation, query)),
                    limit,
                ))
            }
        }
    }

    /// Same result as [`GitEvidenceProjectionV1::session_ids_for_scope`]:
    /// `None` for an empty filter, otherwise the authoritative (possibly
    /// empty) intersection of every scoped selector.
    #[hotpath::measure(label = "sessions.git_correlation.graph_view.session_ids_for_scope")]
    pub fn session_ids_for_scope(
        &self,
        filter: &GitScopeFilter,
    ) -> Result<Option<Vec<(String, String)>>, GitCorrelationError> {
        if filter.is_empty() {
            return Ok(None);
        }
        // Selectors intersect on session digests first, so only sessions in
        // the final answer are ever hydrated.
        let mut selected: Option<BTreeSet<String>> = None;
        if let Some(branch) = &filter.branch {
            selected = Some(intersect_digests(
                selected,
                self.hub_session_digests(branch_entity_id(branch)?, BRANCH_SPAN_RELATION)?,
            ));
        }
        if let Some(worktree) = &filter.worktree {
            selected = Some(intersect_digests(
                selected,
                self.hub_session_digests(worktree_entity_id(worktree)?, WORKTREE_SPAN_RELATION)?,
            ));
        }
        let commit_identities = match &filter.commit {
            Some(commit) => {
                let records = self.commit_records_with_prefix(commit)?;
                let identities = commit_identities_with_producer_fallback(records.iter());
                selected = Some(intersect_digests(
                    selected,
                    identities
                        .keys()
                        .map(|session_id| stable_digest(session_id))
                        .collect(),
                ));
                Some(identities)
            }
            None => None,
        };
        let selected = selected.unwrap_or_default();
        let identities = match commit_identities {
            // Commit records already carry the session identity and its
            // canonical provider, which the intersection above filtered.
            Some(identities) => identities
                .into_iter()
                .filter(|(session_id, _)| selected.contains(&stable_digest(session_id)))
                .collect(),
            None => selected
                .iter()
                .map(|digest| self.session_identity(digest))
                .collect::<Result<BTreeMap<_, _>, _>>()?,
        };
        Ok(Some(scope_session_ids(Some(identities))))
    }

    /// Resolves a single branch or worktree selector only far enough for a
    /// caller to detect that its own session bound was exceeded. Compound and
    /// commit selectors retain the complete intersection semantics above.
    #[hotpath::measure(label = "sessions.git_correlation.graph_view.session_ids_for_scope_bounded")]
    pub fn session_ids_for_scope_bounded(
        &self,
        filter: &GitScopeFilter,
        maximum: usize,
    ) -> Result<Option<Vec<(String, String)>>, GitCorrelationError> {
        let (hub, relation_kind, git_ref) = match (&filter.branch, &filter.worktree, &filter.commit)
        {
            (Some(branch), None, None) => (
                branch_entity_id(branch)?,
                BRANCH_SPAN_RELATION,
                GitRefFilter::Branch(branch.clone()),
            ),
            (None, Some(worktree), None) => (
                worktree_entity_id(worktree)?,
                WORKTREE_SPAN_RELATION,
                GitRefFilter::Worktree(worktree.clone()),
            ),
            _ => return self.session_ids_for_scope(filter),
        };
        let query = SessionsForQuery {
            git_ref,
            since: None,
            until: None,
            limit: maximum,
        };
        let identities = self
            .leading_sessions(hub, relation_kind, &query, maximum)?
            .into_iter()
            .map(|digest| self.session_identity(&digest))
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        Ok(Some(scope_session_ids(Some(identities))))
    }

    fn span_hits_from_hub(
        &self,
        hub: GraphEntityId,
        relation_kind: &str,
        query: &SessionsForQuery,
    ) -> Result<Vec<SessionGitCorrelationHit>, GitCorrelationError> {
        let limit = sessions_for_limit(query);
        let sessions = self.leading_sessions(hub, relation_kind, query, limit)?;
        let mut spans = Vec::new();
        for session_digest in &sessions {
            spans.extend(self.session_spans(session_digest)?);
        }
        Ok(span_hits(
            spans.iter().filter(|span| span_matches_query(span, query)),
            limit,
        ))
    }

    /// Digests of every session that can rank among the first `limit` hits:
    /// the hub's span index is paged newest-`last_ts` first, so the first
    /// matching span seen for a session carries that session's sort key. Paging
    /// stops once `limit` sessions are known and the next span can no longer
    /// tie the `limit`-th session's key, or once spans end before `since`.
    fn leading_sessions(
        &self,
        hub: GraphEntityId,
        relation_kind: &str,
        query: &SessionsForQuery,
        limit: usize,
    ) -> Result<Vec<String>, GitCorrelationError> {
        let kinds = BTreeSet::from([GraphRelationKind::new(relation_kind)?]);
        let starts = [hub];
        let mut selected = Vec::new();
        let mut seen = BTreeSet::new();
        let mut boundary: Option<i64> = None;
        let mut after: Option<GraphRelationId> = None;
        loop {
            let page = single_batch(self.snapshot.outgoing_relation_ids_page(
                &starts,
                &kinds,
                after.as_ref(),
                HUB_FANOUT_PAGE_ITEMS,
                Arc::clone(&self.cancellation),
            )?)?;
            for identity in &page {
                let key = SpanIndexKey::parse(identity)?;
                if query.since.is_some_and(|since| key.last_ts < since)
                    || boundary.is_some_and(|boundary| key.last_ts < boundary)
                {
                    return Ok(selected);
                }
                if query.until.is_some_and(|until| key.first_ts > until) {
                    continue;
                }
                if seen.insert(key.session_digest.clone()) {
                    selected.push(key.session_digest);
                    if selected.len() == limit {
                        boundary = Some(key.last_ts);
                    }
                }
            }
            if page.len() < HUB_FANOUT_PAGE_ITEMS {
                return Ok(selected);
            }
            after = page.last().cloned();
        }
    }

    /// Every span of one session, hydrated through its own fan-out.
    fn session_spans(
        &self,
        session_digest: &str,
    ) -> Result<Vec<SessionGitSpan>, GitCorrelationError> {
        let kinds = BTreeSet::from([GraphRelationKind::new(SESSION_SPAN_RELATION)?]);
        let mut targets = Vec::new();
        self.snapshot.visit_outgoing_relation_targets(
            &GraphEntityId::new(format!("session:{session_digest}"))?,
            &kinds,
            Arc::clone(&self.cancellation),
            &mut |target| targets.push(target.target),
        )?;
        targets
            .iter()
            .map(|span| {
                serde_json::from_slice(required_bytes_property(
                    &span.properties,
                    SPAN_RECORD_PROPERTY,
                    "span record",
                )?)
                .map_err(Into::into)
            })
            .collect()
    }

    /// Digests of the distinct sessions with at least one span under `hub`,
    /// read from the index keys alone: no span or session payload is decoded.
    fn hub_session_digests(
        &self,
        hub: GraphEntityId,
        relation_kind: &str,
    ) -> Result<BTreeSet<String>, GitCorrelationError> {
        let kinds = BTreeSet::from([GraphRelationKind::new(relation_kind)?]);
        let starts = [hub];
        let mut digests = BTreeSet::new();
        let mut after: Option<GraphRelationId> = None;
        loop {
            let page = single_batch(self.snapshot.outgoing_relation_ids_page(
                &starts,
                &kinds,
                after.as_ref(),
                HUB_FANOUT_PAGE_ITEMS,
                Arc::clone(&self.cancellation),
            )?)?;
            for identity in &page {
                digests.insert(SpanIndexKey::parse(identity)?.session_digest);
            }
            if page.len() < HUB_FANOUT_PAGE_ITEMS {
                return Ok(digests);
            }
            after = page.last().cloned();
        }
    }

    /// The session identity and canonical provider behind one session digest.
    fn session_identity(&self, digest: &str) -> Result<(String, String), GitCorrelationError> {
        let session = self
            .snapshot
            .entity(
                &GraphEntityRef::new(
                    self.snapshot.projection().clone(),
                    GraphEntityId::new(format!("session:{digest}"))?,
                ),
                Arc::clone(&self.cancellation),
            )?
            .ok_or_else(|| {
                GitCorrelationError::Corrupt(
                    "Git evidence span index names an absent session".to_owned(),
                )
            })?;
        Ok((
            required_string_property(&session.properties, SESSION_ID_PROPERTY, "session id")?
                .to_owned(),
            required_string_property(&session.properties, PROVIDER_PROPERTY, "provider")?
                .to_owned(),
        ))
    }

    /// Every commit/session record whose commit SHA starts with `sha`, in
    /// canonical projection order.
    fn commit_records_with_prefix(
        &self,
        sha: &str,
    ) -> Result<Vec<CommitSessionRecord>, GitCorrelationError> {
        let bucket = commit_prefix_entity_id(commit_prefix(sha)?)?;
        let prefix_kinds = BTreeSet::from([GraphRelationKind::new(COMMIT_PREFIX_COMMIT_RELATION)?]);
        let mut commits = Vec::new();
        self.snapshot.visit_outgoing_relation_targets(
            &bucket,
            &prefix_kinds,
            Arc::clone(&self.cancellation),
            &mut |target| commits.push(target.target),
        )?;
        let record_kinds = BTreeSet::from([GraphRelationKind::new(SESSION_COMMIT_RELATION)?]);
        let mut records = Vec::new();
        for commit in commits {
            let commit_sha =
                required_string_property(&commit.properties, COMMIT_SHA_PROPERTY, "commit sha")?;
            if !commit_sha.starts_with(sha) {
                continue;
            }
            let starts = [commit.identity.clone()];
            let mut after: Option<GraphRelationId> = None;
            loop {
                let page = single_batch(self.snapshot.incoming_relation_ids_page(
                    &starts,
                    &record_kinds,
                    after.as_ref(),
                    HUB_FANOUT_PAGE_ITEMS,
                    Arc::clone(&self.cancellation),
                )?)?;
                for identity in &page {
                    let relation = self
                        .snapshot
                        .relation(
                            &GraphRelationRef::new(
                                self.snapshot.projection().clone(),
                                identity.clone(),
                            ),
                            Arc::clone(&self.cancellation),
                        )?
                        .ok_or_else(|| {
                            GitCorrelationError::Corrupt(
                                "Git evidence commit fan-out names an absent relation".to_owned(),
                            )
                        })?;
                    records.push(serde_json::from_slice::<CommitSessionRecord>(
                        required_bytes_property(
                            &relation.properties,
                            COMMIT_RECORD_PROPERTY,
                            "commit evidence record",
                        )?,
                    )?);
                }
                if page.len() < HUB_FANOUT_PAGE_ITEMS {
                    break;
                }
                after = page.last().cloned();
            }
        }
        records.sort_by(commit_record_order);
        Ok(records)
    }
}

/// Relation identity of one branch- or worktree-hub → span index edge.
///
/// The graph pages a hub's relations in ascending identity order, so the key
/// leads with the bitwise-inverted order-preserving encoding of `last_ts`:
/// newest activity first. `first_ts` follows so an `until` bound is decided
/// from the key alone, then the session digest so paging counts distinct
/// sessions without hydrating a span, and the span digest keeps the key
/// unique.
pub(super) struct SpanIndexKey {
    pub(super) last_ts: i64,
    pub(super) first_ts: i64,
    pub(super) session_digest: String,
}

impl SpanIndexKey {
    pub(super) fn encode(kind: &str, span: &SessionGitSpan) -> String {
        format!(
            "{kind}:{:016x}:{:016x}:{}:{}",
            !order_preserving_bits(span.last_ts),
            order_preserving_bits(span.first_ts),
            stable_digest(&span.session_id),
            stable_digest(&span.span_id),
        )
    }

    pub(super) fn parse(identity: &GraphRelationId) -> Result<Self, GitCorrelationError> {
        let corrupt = || {
            GitCorrelationError::Corrupt(format!(
                "Git evidence span index relation `{identity}` is malformed"
            ))
        };
        let mut parts = identity.as_str().split(':');
        let (Some(_kind), Some(last_ts), Some(first_ts), Some(session_digest), Some(_), None) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            return Err(corrupt());
        };
        Ok(Self {
            last_ts: i64_from_order_preserving_bits(
                !u64::from_str_radix(last_ts, 16).map_err(|_| corrupt())?,
            ),
            first_ts: i64_from_order_preserving_bits(
                u64::from_str_radix(first_ts, 16).map_err(|_| corrupt())?,
            ),
            session_digest: session_digest.to_owned(),
        })
    }
}

/// Maps an `i64` onto `u64` so unsigned (and therefore zero-padded hex
/// lexicographic) order equals signed order.
fn order_preserving_bits(value: i64) -> u64 {
    (value as u64) ^ (1 << 63)
}

fn i64_from_order_preserving_bits(bits: u64) -> i64 {
    (bits ^ (1 << 63)) as i64
}

fn intersect_digests(
    accumulated: Option<BTreeSet<String>>,
    next: BTreeSet<String>,
) -> BTreeSet<String> {
    match accumulated {
        Some(existing) => existing.intersection(&next).cloned().collect(),
        None => next,
    }
}

fn single_batch<T>(mut batches: Vec<Vec<T>>) -> Result<Vec<T>, GitCorrelationError> {
    match batches.len() {
        1 => Ok(batches.swap_remove(0)),
        _ => Err(GitCorrelationError::Corrupt(
            "graph fan-out answered a single start with a foreign batch shape".to_owned(),
        )),
    }
}

fn string_property<'a>(
    properties: &'a BTreeMap<GraphPropertyName, GraphProperty>,
    name: &str,
) -> Result<Option<&'a str>, GitCorrelationError> {
    match properties.get(&GraphPropertyName::new(name)?) {
        Some(GraphProperty::String(value)) => Ok(Some(value.as_str())),
        Some(_) => Err(GitCorrelationError::Corrupt(format!(
            "verified Git evidence property `{name}` is not a string"
        ))),
        None => Ok(None),
    }
}

fn required_string_property<'a>(
    properties: &'a BTreeMap<GraphPropertyName, GraphProperty>,
    name: &str,
    description: &str,
) -> Result<&'a str, GitCorrelationError> {
    string_property(properties, name)?.ok_or_else(|| {
        GitCorrelationError::Corrupt(format!(
            "verified Git evidence is missing its {description}"
        ))
    })
}

fn required_bytes_property<'a>(
    properties: &'a BTreeMap<GraphPropertyName, GraphProperty>,
    name: &str,
    description: &str,
) -> Result<&'a [u8], GitCorrelationError> {
    match properties.get(&GraphPropertyName::new(name)?) {
        Some(GraphProperty::Bytes(bytes)) => Ok(bytes.as_slice()),
        _ => Err(GitCorrelationError::Corrupt(format!(
            "verified Git evidence is missing its {description}"
        ))),
    }
}

fn required_count_property(
    properties: &BTreeMap<GraphPropertyName, GraphProperty>,
    name: &str,
) -> Result<u64, GitCorrelationError> {
    match properties.get(&GraphPropertyName::new(name)?) {
        Some(GraphProperty::I64(count)) => u64::try_from(*count).map_err(|_| {
            GitCorrelationError::Corrupt(format!(
                "verified Git evidence records a negative `{name}`"
            ))
        }),
        _ => Err(GitCorrelationError::Corrupt(format!(
            "verified Git evidence is missing its `{name}` metadata"
        ))),
    }
}

fn require_git_evidence_projection_identity(
    identity: &GraphProjectionIdentity,
) -> Result<(), GitCorrelationError> {
    if identity.namespace.as_str() != GIT_EVIDENCE_NAMESPACE
        || identity.projection.as_str() != GIT_EVIDENCE_PROJECTION
    {
        return Err(GitCorrelationError::Corrupt(
            "verified Git evidence uses a foreign projection identity".to_owned(),
        ));
    }
    Ok(())
}

fn require_git_evidence_generation(
    snapshot: &VerifiedGraphSnapshot,
    projection: &GitEvidenceProjectionV1,
    projector_revision: &GraphProjectorRevision,
) -> Result<(), GitCorrelationError> {
    require_git_evidence_projection_identity(snapshot.projection())?;
    let expected = git_evidence_generation_id(projection, projector_revision)?;
    if snapshot.generation() != &expected {
        return Err(GitCorrelationError::Corrupt(format!(
            "verified Git evidence generation mismatch: expected `{expected}`, observed `{}`",
            snapshot.generation()
        )));
    }
    Ok(())
}

fn projection_entity_id() -> Result<GraphEntityId, GitCorrelationError> {
    GraphEntityId::new("projection:session-git-evidence").map_err(Into::into)
}

fn projection_entity(
    projection: &GitEvidenceProjectionV1,
    projector_revision: &GraphProjectorRevision,
) -> Result<GraphEntity, GitCorrelationError> {
    let count = |rows: usize, family: &str| {
        i64::try_from(rows).map_err(|_| {
            GitCorrelationError::Contract(format!("Git evidence {family} count exceeds i64"))
        })
    };
    GraphEntity::new(
        projection_entity_id()?,
        BTreeSet::new(),
        BTreeMap::from([
            (
                GraphPropertyName::new(PROJECTION_RECORD_PROPERTY)?,
                GraphProperty::String(projection.source_watermark().to_owned()),
            ),
            (
                GraphPropertyName::new(PROJECTOR_REVISION_PROPERTY)?,
                GraphProperty::String(projector_revision.as_str().to_owned()),
            ),
            (
                GraphPropertyName::new(SPAN_COUNT_PROPERTY)?,
                GraphProperty::I64(count(projection.spans().len(), "span")?),
            ),
            (
                GraphPropertyName::new(COMMIT_COUNT_PROPERTY)?,
                GraphProperty::I64(count(projection.commit_sessions().len(), "commit")?),
            ),
        ]),
    )
    .map_err(Into::into)
}

fn session_entity(session_id: &str, provider: &str) -> Result<GraphEntity, GitCorrelationError> {
    GraphEntity::new(
        session_entity_id(session_id)?,
        BTreeSet::from([GraphLabel::new(SESSION_LABEL)?]),
        BTreeMap::from([
            (
                GraphPropertyName::new(SESSION_ID_PROPERTY)?,
                GraphProperty::String(session_id.to_owned()),
            ),
            (
                GraphPropertyName::new(PROVIDER_PROPERTY)?,
                GraphProperty::String(provider.to_owned()),
            ),
        ]),
    )
    .map_err(Into::into)
}

fn branch_entity(branch: &str) -> Result<GraphEntity, GitCorrelationError> {
    GraphEntity::new(
        branch_entity_id(branch)?,
        BTreeSet::from([GraphLabel::new(BRANCH_LABEL)?]),
        BTreeMap::from([(
            GraphPropertyName::new(BRANCH_PROPERTY)?,
            GraphProperty::String(branch.to_owned()),
        )]),
    )
    .map_err(Into::into)
}

fn worktree_entity(worktree: &str) -> Result<GraphEntity, GitCorrelationError> {
    GraphEntity::new(
        worktree_entity_id(worktree)?,
        BTreeSet::from([GraphLabel::new(WORKTREE_LABEL)?]),
        BTreeMap::from([(
            GraphPropertyName::new(WORKTREE_PROPERTY)?,
            GraphProperty::String(worktree.to_owned()),
        )]),
    )
    .map_err(Into::into)
}

fn commit_prefix_entity(prefix: &str) -> Result<GraphEntity, GitCorrelationError> {
    GraphEntity::new(
        commit_prefix_entity_id(prefix)?,
        BTreeSet::from([GraphLabel::new(COMMIT_PREFIX_LABEL)?]),
        BTreeMap::from([(
            GraphPropertyName::new(COMMIT_PREFIX_PROPERTY)?,
            GraphProperty::String(prefix.to_owned()),
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

/// Hub → span index edge whose identity is the span's [`SpanIndexKey`].
fn span_index_relation(
    projection: &GraphProjectionIdentity,
    hub: GraphEntityId,
    relation_kind: &str,
    index_kind: &str,
    span: &SessionGitSpan,
) -> Result<GraphGenerationRelation, GitCorrelationError> {
    GraphGenerationRelation::new(
        GraphRelationId::new(SpanIndexKey::encode(index_kind, span))?,
        GraphEntityRef::new(projection.clone(), hub),
        GraphEntityRef::new(projection.clone(), span_entity_id(&span.span_id)?),
        GraphRelationKind::new(relation_kind)?,
        BTreeMap::new(),
    )
    .map_err(Into::into)
}

fn commit_prefix_relation(
    projection: &GraphProjectionIdentity,
    prefix: &str,
    commit_sha: &str,
) -> Result<GraphGenerationRelation, GitCorrelationError> {
    GraphGenerationRelation::new(
        GraphRelationId::new(format!("commit-prefix-commit:{commit_sha}"))?,
        GraphEntityRef::new(projection.clone(), commit_prefix_entity_id(prefix)?),
        GraphEntityRef::new(projection.clone(), commit_entity_id(commit_sha)?),
        GraphRelationKind::new(COMMIT_PREFIX_COMMIT_RELATION)?,
        BTreeMap::new(),
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

fn branch_entity_id(branch: &str) -> Result<GraphEntityId, GitCorrelationError> {
    GraphEntityId::new(stable_identity("branch", branch)).map_err(Into::into)
}

fn worktree_entity_id(worktree: &str) -> Result<GraphEntityId, GitCorrelationError> {
    GraphEntityId::new(stable_identity("worktree", worktree)).map_err(Into::into)
}

fn commit_prefix_entity_id(prefix: &str) -> Result<GraphEntityId, GitCorrelationError> {
    GraphEntityId::new(format!("commit-prefix:{prefix}")).map_err(Into::into)
}

/// The six-digit bucket a commit SHA (or an admitted query prefix) files
/// under. Both sides pass [`super::parse_commit_sha`], so anything shorter
/// is a contract violation, not a query miss.
fn commit_prefix(sha: &str) -> Result<&str, GitCorrelationError> {
    sha.get(..COMMIT_PREFIX_LEN).ok_or_else(|| {
        GitCorrelationError::Contract(format!(
            "commit `{sha}` is shorter than the {COMMIT_PREFIX_LEN}-digit index prefix"
        ))
    })
}

fn stable_identity(kind: &str, material: &str) -> String {
    format!("{kind}:{}", stable_digest(material))
}

fn stable_digest(material: &str) -> String {
    hex::encode(Sha256::digest(material.as_bytes()))
}

struct AtomicGraphCancellation(Arc<AtomicBool>);

impl GraphCancellation for AtomicGraphCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

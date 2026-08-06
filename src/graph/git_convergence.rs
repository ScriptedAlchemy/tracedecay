use super::{
    COMMIT_FRONTIER_PROPERTY, GitReferenceRecord, GitTopologyError, GitTopologyFreshness,
    GitTopologyResult, GitTopologyState, GitTopologyStore, MAX_GIT_PARENTS,
    PENDING_GENERATION_PROPERTY, PENDING_REF_FRONTIER_PROPERTY, REF_FRONTIER_PROPERTY,
    REPOSITORY_ENTITY, freshness_entity, git_oid, measured_throughput, namespace, object_entity,
    object_entity_id, parent_relation, parse_object_id, projection, reference_entity,
    reference_entity_id, reference_target_relation,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use gix::bstr::ByteSlice as _;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    GitGraphEvidenceIntent, GitGraphEvidencePublicationReceipt, GitOidV1, ProjectId,
};
use tracedecay_graph_db::{
    GraphDb, GraphEntity, GraphEntityId, GraphLabel, GraphMutation, GraphProperty,
    GraphPropertyName, GraphWatermark, GraphWriteBatch, NeverCancelled, SourceGeneration,
};

pub trait GitEvidenceReceiptSink: Send + Sync {
    fn acknowledge<'a>(
        &'a self,
        intent: &'a GitGraphEvidenceIntent,
        receipt: GitGraphEvidencePublicationReceipt,
    ) -> Pin<Box<dyn Future<Output = Result<(), GitTopologyError>> + Send + 'a>>;
}

struct PendingEvidencePublication {
    intent: GitGraphEvidenceIntent,
    sink: Arc<dyn GitEvidenceReceiptSink>,
    receipt: Option<GitGraphEvidencePublicationReceipt>,
    retry_delay: std::time::Duration,
}

#[derive(Default)]
struct PendingEvidencePublications {
    active: Option<tracedecay_domain::ContentDigest>,
    pending_digests: BTreeSet<tracedecay_domain::ContentDigest>,
    queue: VecDeque<PendingEvidencePublication>,
}

pub struct GitTopologyConvergenceOwner {
    project: ProjectId,
    roots: Arc<tokio::sync::Mutex<PendingWorktreeRoots>>,
    evidence: Arc<tokio::sync::Mutex<PendingEvidencePublications>>,
    wake: Arc<tokio::sync::Notify>,
    cancelled: Arc<AtomicBool>,
    started_jobs: Arc<AtomicU64>,
    opened_repositories: Arc<AtomicU64>,
    completed_slices: Arc<AtomicU64>,
    join: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

#[derive(Default)]
struct PendingWorktreeRoots {
    active: Option<PathBuf>,
    pending: BTreeSet<PathBuf>,
}

impl GitTopologyConvergenceOwner {
    pub fn start(project: ProjectId, database: Arc<GraphDb>) -> Arc<Self> {
        let roots = Arc::new(tokio::sync::Mutex::new(PendingWorktreeRoots::default()));
        let evidence = Arc::new(tokio::sync::Mutex::new(
            PendingEvidencePublications::default(),
        ));
        let wake = Arc::new(tokio::sync::Notify::new());
        let cancelled = Arc::new(AtomicBool::new(false));
        let started_jobs = Arc::new(AtomicU64::new(0));
        let opened_repositories = Arc::new(AtomicU64::new(0));
        let completed_slices = Arc::new(AtomicU64::new(0));
        let worker_roots = Arc::clone(&roots);
        let worker_evidence = Arc::clone(&evidence);
        let worker_wake = Arc::clone(&wake);
        let worker_cancelled = Arc::clone(&cancelled);
        let worker_started_jobs = Arc::clone(&started_jobs);
        let worker_opened_repositories = Arc::clone(&opened_repositories);
        let worker_completed_slices = Arc::clone(&completed_slices);
        let worker_project = project.clone();
        let join = tokio::spawn(async move {
            'owner: loop {
                worker_wake.notified().await;
                loop {
                    if worker_cancelled.load(Ordering::Acquire) {
                        break 'owner;
                    }
                    let root = {
                        let mut roots = worker_roots.lock().await;
                        let next = roots.pending.pop_first();
                        roots.active.clone_from(&next);
                        next
                    };
                    if let Some(root) = root {
                        worker_started_jobs.fetch_add(1, Ordering::AcqRel);
                        if let Err(error) = run_incremental_convergence(
                            worker_project.clone(),
                            root,
                            Arc::clone(&database),
                            Arc::clone(&worker_cancelled),
                            Arc::clone(&worker_opened_repositories),
                            Arc::clone(&worker_completed_slices),
                        )
                        .await
                        {
                            publish_failed_status(&worker_project, Arc::clone(&database), &error)
                                .await;
                            tracing::warn!(
                                event = "git_topology_convergence",
                                outcome = "unavailable",
                                error = %error,
                            );
                        }
                        worker_roots.lock().await.active = None;
                        continue;
                    }
                    let pending = {
                        let mut evidence = worker_evidence.lock().await;
                        let next = evidence.queue.pop_front();
                        if let Some(next) = &next {
                            evidence.pending_digests.remove(next.intent.intent_digest());
                            evidence.active = Some(next.intent.intent_digest().clone());
                        }
                        next
                    };
                    let Some(pending) = pending else {
                        break;
                    };
                    let mut pending = pending;
                    let published = publish_evidence_intent(
                        &worker_project,
                        Arc::clone(&database),
                        &mut pending,
                    )
                    .await;
                    let retry_delay = pending.retry_delay;
                    {
                        let mut evidence = worker_evidence.lock().await;
                        evidence.active = None;
                        if !published && !worker_cancelled.load(Ordering::Acquire) {
                            evidence
                                .pending_digests
                                .insert(pending.intent.intent_digest().clone());
                            evidence.queue.push_back(pending);
                        }
                    }
                    if !published {
                        tokio::select! {
                            () = tokio::time::sleep(retry_delay) => {}
                            () = worker_wake.notified() => {}
                        }
                    }
                }
            }
        });
        Arc::new(Self {
            project,
            roots,
            evidence,
            wake,
            cancelled,
            started_jobs,
            opened_repositories,
            completed_slices,
            join: tokio::sync::Mutex::new(Some(join)),
        })
    }

    pub async fn wake(&self, canonical_project_root: &Path) -> GitTopologyResult<()> {
        const MAX_PENDING_WORKTREES: usize = 4_096;

        let mut roots = self.roots.lock().await;
        if roots.active.as_deref() == Some(canonical_project_root)
            || roots.pending.contains(canonical_project_root)
        {
            return Ok(());
        }
        if roots.pending.len() == MAX_PENDING_WORKTREES {
            return Err(GitTopologyError::Contract(format!(
                "Git topology pending worktree budget {MAX_PENDING_WORKTREES} exceeded"
            )));
        }
        roots.pending.insert(canonical_project_root.to_path_buf());
        drop(roots);
        self.wake.notify_one();
        Ok(())
    }

    pub async fn enqueue_evidence(
        &self,
        canonical_project_root: &Path,
        intent: GitGraphEvidenceIntent,
        sink: Arc<dyn GitEvidenceReceiptSink>,
    ) -> GitTopologyResult<()> {
        const MAX_PENDING_EVIDENCE: usize = 10_000;

        intent
            .validate()
            .map_err(|error| GitTopologyError::Contract(error.to_string()))?;
        if intent.project_id() != &self.project {
            return Err(GitTopologyError::Contract(
                "Git evidence intent belongs to another convergence owner".to_owned(),
            ));
        }
        let mut evidence = self.evidence.lock().await;
        if evidence.active.as_ref() == Some(intent.intent_digest())
            || evidence.pending_digests.contains(intent.intent_digest())
        {
            return Ok(());
        }
        if evidence.queue.len() >= MAX_PENDING_EVIDENCE {
            return Err(GitTopologyError::Contract(format!(
                "Git evidence pending budget {MAX_PENDING_EVIDENCE} exceeded"
            )));
        }
        evidence
            .pending_digests
            .insert(intent.intent_digest().clone());
        evidence.queue.push_back(PendingEvidencePublication {
            intent,
            sink,
            receipt: None,
            retry_delay: std::time::Duration::from_millis(50),
        });
        drop(evidence);
        self.wake(canonical_project_root).await
    }

    pub async fn shutdown(&self) -> GitTopologyResult<()> {
        self.cancelled.store(true, Ordering::Release);
        self.wake.notify_one();
        if let Some(join) = self.join.lock().await.take() {
            join.await.map_err(|error| {
                GitTopologyError::Repository(format!(
                    "Git convergence owner shutdown join failed: {error}"
                ))
            })?;
        }
        tracing::debug!(
            event = "git_topology_convergence_owner_shutdown",
            started_jobs = self.started_jobs.load(Ordering::Acquire),
            opened_repositories = self.opened_repositories.load(Ordering::Acquire),
            completed_slices = self.completed_slices.load(Ordering::Acquire),
        );
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn started_jobs(&self) -> u64 {
        self.started_jobs.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(super) fn opened_repositories(&self) -> u64 {
        self.opened_repositories.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(super) fn completed_slices(&self) -> u64 {
        self.completed_slices.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(super) async fn is_shutdown(&self) -> bool {
        self.join.lock().await.is_none()
    }
}

async fn publish_failed_status(
    project: &ProjectId,
    database: Arc<GraphDb>,
    error: &GitTopologyError,
) {
    let store = GitTopologyStore::new(database);
    let status_project = project.clone();
    let reason = error.to_string();
    let status_result = tokio::task::spawn_blocking(move || {
        store.write_freshness(
            &status_project,
            &GitTopologyFreshness {
                state: GitTopologyState::Failed,
                processed_commits: 0,
                remaining_lower_bound: None,
                throughput_per_second: None,
                eta_seconds_range: None,
                last_watermark: None,
                generation: None,
                reason: Some(reason),
            },
        )
    })
    .await;
    if let Err(status_error) = status_result
        .map_err(|join_error| {
            GitTopologyError::Repository(format!("Git failure status task failed: {join_error}"))
        })
        .and_then(|result| result)
    {
        tracing::warn!(
            event = "git_topology_convergence_status",
            outcome = "unavailable",
            error = %status_error,
        );
    }
}

async fn publish_evidence_intent(
    project: &ProjectId,
    database: Arc<GraphDb>,
    pending: &mut PendingEvidencePublication,
) -> bool {
    if pending.receipt.is_none() {
        let store = GitTopologyStore::new(database);
        let project = project.clone();
        let intent = pending.intent.clone();
        let publication = tokio::task::spawn_blocking(move || {
            store.publish_evidence(&project, std::slice::from_ref(&intent))
        })
        .await
        .map_err(|error| {
            GitTopologyError::Repository(format!("Git evidence publication task failed: {error}"))
        })
        .and_then(|result| result);
        match publication {
            Ok(mut receipts) if receipts.len() == 1 => pending.receipt = receipts.pop(),
            Ok(receipts) => {
                tracing::warn!(
                    event = "git_evidence_publication",
                    outcome = "invalid_receipt_cardinality",
                    receipts = receipts.len(),
                );
                pending.retry_delay = next_evidence_retry_delay(pending.retry_delay);
                return false;
            }
            Err(error) => {
                tracing::debug!(
                    event = "git_evidence_publication",
                    outcome = "pending",
                    error = %error,
                );
                pending.retry_delay = next_evidence_retry_delay(pending.retry_delay);
                return false;
            }
        }
    }
    let Some(receipt) = pending.receipt.clone() else {
        tracing::warn!(
            event = "git_evidence_publication",
            outcome = "missing_receipt",
        );
        pending.retry_delay = next_evidence_retry_delay(pending.retry_delay);
        return false;
    };
    if let Err(error) = pending.sink.acknowledge(&pending.intent, receipt).await {
        tracing::warn!(
            event = "git_evidence_acknowledgement",
            outcome = "pending",
            error = %error,
        );
        pending.retry_delay = next_evidence_retry_delay(pending.retry_delay);
        return false;
    }
    true
}

fn next_evidence_retry_delay(current: std::time::Duration) -> std::time::Duration {
    current
        .saturating_mul(2)
        .min(std::time::Duration::from_secs(5))
}

pub(super) async fn run_incremental_convergence(
    project: ProjectId,
    project_root: std::path::PathBuf,
    database: Arc<GraphDb>,
    cancelled: Arc<AtomicBool>,
    opened_repositories: Arc<AtomicU64>,
    completed_slices: Arc<AtomicU64>,
) -> GitTopologyResult<()> {
    let started = std::time::Instant::now();
    let mut processed = 0_usize;
    let mut last_watermark = None;
    let mut generation = None;
    let mut remaining_lower_bound = None;
    let mut repository = tokio::task::spawn_blocking({
        let project_root = project_root.clone();
        move || open_repository(&project_root)
    })
    .await
    .map_err(|error| {
        GitTopologyError::Repository(format!("Git convergence open task failed: {error}"))
    })??;
    opened_repositories.fetch_add(1, Ordering::AcqRel);
    loop {
        if cancelled.load(Ordering::Acquire) {
            let store = GitTopologyStore::new(Arc::clone(&database));
            let project = project.clone();
            tokio::task::spawn_blocking(move || {
                store.write_freshness(
                    &project,
                    &GitTopologyFreshness {
                        state: GitTopologyState::Partial,
                        processed_commits: saturating_u64(processed),
                        remaining_lower_bound,
                        throughput_per_second: measured_throughput(processed, started.elapsed()),
                        eta_seconds_range: None,
                        last_watermark,
                        generation,
                        reason: Some("shutdown".to_owned()),
                    },
                )
            })
            .await
            .map_err(|error| {
                GitTopologyError::Repository(format!(
                    "Git convergence cancellation status task failed: {error}"
                ))
            })??;
            return Ok(());
        }
        let slice_project = project.clone();
        let slice_database = Arc::clone(&database);
        let (returned_repository, outcome) = tokio::task::spawn_blocking(move || {
            let outcome = converge_slice_with_repository(
                &slice_project,
                &mut repository,
                slice_database,
                processed,
                started,
            );
            (repository, outcome)
        })
        .await
        .map_err(|error| {
            GitTopologyError::Repository(format!("Git convergence slice failed: {error}"))
        })?;
        repository = returned_repository;
        let outcome = outcome?;
        completed_slices.fetch_add(1, Ordering::AcqRel);
        processed = processed.saturating_add(outcome.processed);
        last_watermark = outcome.last_watermark.clone();
        generation = Some(outcome.generation.clone());
        remaining_lower_bound = (!outcome.done).then_some(saturating_u64(outcome.frontier_len));
        tracing::debug!(
            event = "git_topology_convergence_slice",
            processed = outcome.processed,
            frontier = outcome.frontier_len,
            elapsed_micros = outcome.elapsed_micros,
        );
        if outcome.done {
            return Ok(());
        }
        tokio::task::yield_now().await;
    }
}

pub(super) struct SliceOutcome {
    pub(super) done: bool,
    pub(super) processed: usize,
    pub(super) frontier_len: usize,
    pub(super) last_watermark: Option<String>,
    pub(super) generation: String,
    pub(super) elapsed_micros: u128,
}

#[cfg(test)]
pub(super) fn converge_slice(
    project: &ProjectId,
    project_root: &Path,
    database: Arc<GraphDb>,
) -> GitTopologyResult<SliceOutcome> {
    let mut repository = open_repository(project_root)?;
    converge_slice_with_repository(
        project,
        &mut repository,
        database,
        0,
        std::time::Instant::now(),
    )
}

fn open_repository(project_root: &Path) -> GitTopologyResult<gix::Repository> {
    let mut repository = gix::open(project_root).map_err(map_open_error)?;
    repository.object_cache_size(Some(8 * 1024 * 1024));
    Ok(repository)
}

fn converge_slice_with_repository(
    project: &ProjectId,
    repository: &mut gix::Repository,
    database: Arc<GraphDb>,
    processed_before: usize,
    run_started: std::time::Instant,
) -> GitTopologyResult<SliceOutcome> {
    const SLICE_COMMITS: usize = 256;
    const SLICE_WALL_BUDGET: std::time::Duration = std::time::Duration::from_millis(50);

    let slice_started = std::time::Instant::now();
    let store = GitTopologyStore::new(database);
    let mut frontier = store.load_frontier(project)?;
    let mut mutations = Vec::new();
    let (current_refs, mut generation) = match &frontier.pending_generation {
        Some(generation) => (frontier.pending_refs.clone(), generation.clone()),
        None => {
            let references = native_reference_frontier(&repository)?;
            let generation = reference_generation(&references);
            (references, generation)
        }
    };
    if frontier.pending_generation.is_none() {
        mutations.extend(reference_delta(&frontier.completed_refs, &current_refs)?);
        frontier.pending_refs = current_refs.clone();
        frontier.pending_generation = Some(generation.clone());
        frontier.commits = changed_ref_roots(&frontier.completed_refs, &current_refs);
    }
    let frontier_objects = frontier
        .commits
        .iter()
        .map(|oid| GitOidV1::new(oid.clone()))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| GitTopologyError::Contract(error.to_string()))?;
    let published_frontier = store.published_object_kinds(project, &frontier_objects)?;
    let mut unpublished_frontier = Vec::with_capacity(frontier.commits.len());
    let mut entities = BTreeSet::new();
    let mut available_ref_targets = BTreeSet::new();
    for (oid, published) in frontier_objects.into_iter().zip(published_frontier) {
        if published.is_some() {
            available_ref_targets.insert(object_entity_id(&oid)?);
            continue;
        }
        let object_id = parse_object_id(&oid)?;
        let object = repository
            .find_object(object_id)
            .map_err(|error| GitTopologyError::Repository(error.to_string()))?;
        if object.kind == gix::object::Kind::Commit {
            unpublished_frontier.push(oid.to_string());
        } else {
            let entity = object_entity(&oid, false)?;
            available_ref_targets.insert(entity.identity.clone());
            push_entity_once(&mut mutations, &mut entities, entity);
        }
    }
    frontier.commits = unpublished_frontier;

    let completed_targets = frontier
        .completed_refs
        .iter()
        .filter_map(|reference| reference.peeled_target.as_ref())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let completed_target_kinds = store.published_object_kinds(project, &completed_targets)?;
    let mut hidden = BTreeSet::new();
    for (target, published) in completed_targets.into_iter().zip(completed_target_kinds) {
        if published == Some(true) {
            hidden.insert(parse_object_id(&target)?);
        }
    }
    let roots = frontier
        .commits
        .iter()
        .map(|oid| GitOidV1::new(oid.clone()))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| GitTopologyError::Contract(error.to_string()))?
        .iter()
        .map(parse_object_id)
        .collect::<GitTopologyResult<Vec<_>>>()?;
    let walk = repository
        .rev_walk(roots)
        .with_hidden(hidden)
        .use_commit_graph(true)
        .sorting(gix::revision::walk::Sorting::BreadthFirst)
        .all()
        .map_err(|error| GitTopologyError::Repository(error.to_string()))?;

    let mut processed_ids = BTreeSet::new();
    let mut next_frontier = frontier.commits.iter().cloned().collect::<BTreeSet<_>>();
    let mut last_watermark = None;
    for info in walk.take(SLICE_COMMITS) {
        if !processed_ids.is_empty() && slice_started.elapsed() >= SLICE_WALL_BUDGET {
            break;
        }
        let info = info.map_err(|error| GitTopologyError::Repository(error.to_string()))?;
        let oid = git_oid(info.id.to_string())?;
        processed_ids.insert(oid.to_string());
        next_frontier.remove(oid.as_str());
        last_watermark = Some(oid.to_string());
        push_entity_once(&mut mutations, &mut entities, object_entity(&oid, true)?);
        let parents = info
            .parent_ids()
            .take(MAX_GIT_PARENTS.saturating_add(1))
            .map(|parent| git_oid(parent.to_string()))
            .collect::<GitTopologyResult<Vec<_>>>()?;
        if parents.len() > MAX_GIT_PARENTS {
            return Err(GitTopologyError::Contract(format!(
                "commit {oid} exceeds the {MAX_GIT_PARENTS} parent budget"
            )));
        }
        let published_parents = store.published_object_kinds(project, &parents)?;
        for (parent, published) in parents.into_iter().zip(published_parents) {
            next_frontier.insert(parent.to_string());
            let parent_id = object_entity_id(&parent)?;
            if entities.contains(&parent_id) || published.is_none() {
                push_entity_once(
                    &mut mutations,
                    &mut entities,
                    object_entity(&parent, false)?,
                );
            }
            mutations.push(GraphMutation::UpsertRelation(parent_relation(
                &oid, &parent,
            )?));
        }
    }
    for reference in &current_refs {
        let Some(target) = &reference.peeled_target else {
            continue;
        };
        let target_id = object_entity_id(target)?;
        if entities.contains(&target_id) || available_ref_targets.contains(&target_id) {
            mutations.push(GraphMutation::UpsertRelation(reference_target_relation(
                reference, target,
            )?));
        }
    }
    next_frontier.retain(|oid| !processed_ids.contains(oid));
    frontier.commits = next_frontier.into_iter().collect();
    let mut done = frontier.commits.is_empty();
    if done {
        let latest_refs = native_reference_frontier(&repository)?;
        let latest_generation = reference_generation(&latest_refs);
        if latest_generation == generation {
            frontier.completed_refs = std::mem::take(&mut frontier.pending_refs);
            frontier.pending_generation = None;
        } else {
            mutations.extend(reference_delta(&current_refs, &latest_refs)?);
            frontier.completed_refs = current_refs;
            frontier.pending_refs = latest_refs;
            generation = latest_generation;
            frontier.pending_generation = Some(generation.clone());
            frontier.commits = changed_ref_roots(&frontier.completed_refs, &frontier.pending_refs);
            done = frontier.commits.is_empty();
            if done {
                frontier.completed_refs = std::mem::take(&mut frontier.pending_refs);
                frontier.pending_generation = None;
            }
        }
    }
    let processed = processed_before.saturating_add(processed_ids.len());
    let frontier_len = frontier.commits.len();
    let state = if done {
        GitTopologyState::Complete
    } else if processed_ids.is_empty() {
        GitTopologyState::Stalled
    } else {
        GitTopologyState::Indexing
    };
    let status_generation = match &frontier.pending_generation {
        Some(generation) => generation.clone(),
        None => generation.clone(),
    };
    let watermark = match last_watermark.as_deref() {
        Some(watermark) => watermark,
        None => "refs-only",
    };
    mutations.push(GraphMutation::UpsertEntity(frontier_entity(&frontier)?));
    mutations.push(GraphMutation::UpsertEntity(freshness_entity(
        &GitTopologyFreshness {
            state,
            processed_commits: saturating_u64(processed),
            remaining_lower_bound: (!done).then_some(saturating_u64(frontier_len)),
            throughput_per_second: measured_throughput(processed, run_started.elapsed()),
            eta_seconds_range: None,
            last_watermark: last_watermark.clone(),
            generation: Some(status_generation.clone()),
            reason: (state == GitTopologyState::Stalled).then(|| "no_progress".to_owned()),
        },
    )?));
    store.database.apply(GraphWriteBatch::new(
        namespace(project)?,
        projection()?,
        SourceGeneration::new(generation.clone())?,
        GraphWatermark::new(format!(
            "git-slice:{}:{}",
            watermark,
            frontier.commits.len()
        ))?,
        mutations,
        Arc::new(NeverCancelled),
    )?)?;
    Ok(SliceOutcome {
        done,
        processed: processed_ids.len(),
        frontier_len,
        last_watermark,
        generation: status_generation,
        elapsed_micros: slice_started.elapsed().as_micros(),
    })
}

fn saturating_u64(value: usize) -> u64 {
    match u64::try_from(value) {
        Ok(value) => value,
        Err(_) => u64::MAX,
    }
}

fn push_entity_once(
    mutations: &mut Vec<GraphMutation>,
    seen: &mut BTreeSet<GraphEntityId>,
    entity: GraphEntity,
) {
    if seen.insert(entity.identity.clone()) {
        mutations.push(GraphMutation::UpsertEntity(entity));
    } else if entity
        .labels
        .iter()
        .any(|label| label.as_str() == "git-commit")
        && let Some(GraphMutation::UpsertEntity(existing)) = mutations.iter_mut().find(|mutation| {
            matches!(
                mutation,
                GraphMutation::UpsertEntity(existing)
                    if existing.identity == entity.identity
            )
        })
    {
        *existing = entity;
    }
}

#[derive(Default)]
pub(super) struct GitFrontierState {
    pub(super) completed_refs: Vec<GitReferenceRecord>,
    pub(super) pending_refs: Vec<GitReferenceRecord>,
    pub(super) commits: Vec<String>,
    pub(super) pending_generation: Option<String>,
}

fn frontier_entity(frontier: &GitFrontierState) -> GitTopologyResult<GraphEntity> {
    let mut properties = BTreeMap::new();
    insert_json_property(
        &mut properties,
        REF_FRONTIER_PROPERTY,
        &frontier.completed_refs,
    )?;
    insert_json_property(
        &mut properties,
        PENDING_REF_FRONTIER_PROPERTY,
        &frontier.pending_refs,
    )?;
    insert_json_property(&mut properties, COMMIT_FRONTIER_PROPERTY, &frontier.commits)?;
    if let Some(generation) = &frontier.pending_generation {
        properties.insert(
            GraphPropertyName::new(PENDING_GENERATION_PROPERTY)?,
            GraphProperty::String(generation.clone()),
        );
    }
    GraphEntity::new(
        GraphEntityId::new(REPOSITORY_ENTITY)?,
        BTreeSet::from([GraphLabel::new("git-repository")?]),
        properties,
    )
    .map_err(Into::into)
}

fn changed_ref_roots(
    previous: &[GitReferenceRecord],
    current: &[GitReferenceRecord],
) -> Vec<String> {
    let previous = previous
        .iter()
        .map(|reference| (reference.name.as_slice(), reference))
        .collect::<BTreeMap<_, _>>();
    current
        .iter()
        .filter(|reference| previous.get(reference.name.as_slice()).copied() != Some(*reference))
        .filter_map(|reference| reference.peeled_target.as_ref())
        .map(ToString::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn insert_json_property<T: Serialize>(
    properties: &mut BTreeMap<GraphPropertyName, GraphProperty>,
    name: &str,
    value: &T,
) -> GitTopologyResult<()> {
    properties.insert(
        GraphPropertyName::new(name)?,
        GraphProperty::String(
            serde_json::to_string(value)
                .map_err(|error| GitTopologyError::Contract(error.to_string()))?,
        ),
    );
    Ok(())
}

fn native_reference_frontier(
    repository: &gix::Repository,
) -> GitTopologyResult<Vec<GitReferenceRecord>> {
    const MAX_REFERENCES: usize = 100_000;
    const MAX_REFERENCE_FRONTIER_BYTES: usize = 4 * 1024 * 1024;

    // HEAD is worktree-local, while this projection is shared by every linked
    // worktree of one project. Dirty/worktree state retains that identity in
    // its own authority; only repository-wide refs belong in this frontier.
    let platform = repository
        .references()
        .map_err(|error| GitTopologyError::Repository(error.to_string()))?;
    let iter = platform
        .all()
        .map_err(|error| GitTopologyError::Repository(error.to_string()))?;
    let mut references = Vec::new();
    let mut frontier_bytes = 0_usize;
    for reference in iter.take(MAX_REFERENCES.saturating_add(1)) {
        if references.len() == MAX_REFERENCES {
            return Err(GitTopologyError::Contract(format!(
                "repository exceeds the {MAX_REFERENCES} reference budget"
            )));
        }
        let mut reference =
            reference.map_err(|error| GitTopologyError::Repository(error.to_string()))?;
        let target = reference.target();
        let direct_target = target
            .try_id()
            .map(|id| git_oid(id.to_string()))
            .transpose()?;
        let symbolic_target = target.try_name().map(|name| name.as_bstr().to_vec());
        let peeled_target = git_oid(
            reference
                .peel_to_id()
                .map_err(|error| GitTopologyError::Repository(error.to_string()))?
                .to_string(),
        )?;
        let record = GitReferenceRecord {
            name: reference.name().as_bstr().to_vec(),
            direct_target,
            peeled_target: Some(peeled_target),
            symbolic_target,
        };
        frontier_bytes = frontier_bytes
            .saturating_add(record.name.len())
            .saturating_add(
                record
                    .direct_target
                    .as_ref()
                    .map_or(0, |target| target.as_str().len()),
            )
            .saturating_add(
                record
                    .peeled_target
                    .as_ref()
                    .map_or(0, |target| target.as_str().len()),
            )
            .saturating_add(record.symbolic_target.as_ref().map_or(0, Vec::len));
        if frontier_bytes > MAX_REFERENCE_FRONTIER_BYTES {
            return Err(GitTopologyError::Contract(format!(
                "repository reference frontier exceeds the {MAX_REFERENCE_FRONTIER_BYTES}-byte budget"
            )));
        }
        references.push(record);
    }
    references.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(references)
}

fn reference_generation(references: &[GitReferenceRecord]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"tracedecay.git-ref-frontier\0");
    for reference in references {
        digest.update(&reference.name);
        digest.update([0]);
        if let Some(target) = &reference.direct_target {
            digest.update(target.as_str().as_bytes());
        }
        digest.update([0]);
        if let Some(target) = &reference.peeled_target {
            digest.update(target.as_str().as_bytes());
        }
        digest.update([0]);
        if let Some(target) = &reference.symbolic_target {
            digest.update(target);
        }
        digest.update([0xff]);
    }
    hex::encode(digest.finalize())
}

fn reference_delta(
    previous: &[GitReferenceRecord],
    current: &[GitReferenceRecord],
) -> GitTopologyResult<Vec<GraphMutation>> {
    let previous = previous
        .iter()
        .map(|reference| (reference.name.as_slice(), reference))
        .collect::<BTreeMap<_, _>>();
    let current = current
        .iter()
        .map(|reference| (reference.name.as_slice(), reference))
        .collect::<BTreeMap<_, _>>();
    let mut mutations = Vec::new();
    for (name, reference) in &previous {
        let next = current.get(name).copied();
        if next == Some(*reference) {
            continue;
        }
        if let Some(target) = &reference.peeled_target {
            mutations.push(GraphMutation::DeleteRelation(
                reference_target_relation(reference, target)?.identity,
            ));
        }
        if next.is_none() {
            mutations.push(GraphMutation::DeleteEntity(reference_entity_id(name)?));
        }
    }
    for (name, reference) in &current {
        if previous.get(name).copied() == Some(*reference) {
            continue;
        }
        mutations.push(GraphMutation::UpsertEntity(reference_entity(reference)?));
    }
    Ok(mutations)
}

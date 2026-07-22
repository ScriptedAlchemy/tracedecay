//! Production proximity evidence over existing read authorities.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use tracedecay_application::RequestContext;
use tracedecay_application::feedback::{
    FeedbackPortFuture, PROXIMITY_CAPABILITY_ID_V1, PROXIMITY_USE_CASE_ID_V1,
    ProximityEvaluationRequestV1,
};
use tracedecay_domain::feedback::{
    FeedbackScopeV1, ProximityAddressV1, ProximityBranchWorktreeIncompatibilityV1,
    ProximityCoverageV1, ProximityRelationStrengthV1, ProximityRiskInputsV1,
    ProximityWarningClassV1,
};
use tracedecay_domain::{
    CanonicalObservationEnvelopeV1, FileOccurrenceId, ObservationScopeV1, UtcMicros,
};
use tracedecay_store::{ObservationProjectionStore, ObservationReplayRequest, ObservationStore};

use super::{
    CanonicalProximityEvidenceAuthorityV1, CanonicalProximityEvidenceBatchV1,
    CanonicalProximityEvidenceV1,
};
use crate::global_db::GlobalDb;
use crate::sessions::git_correlation::{
    GitRefFilter, SessionGitCorrelationHit, SessionsForQuery, normalize_worktree,
};
use crate::store::GlobalDbObservationStore;
use crate::tracedecay::TraceDecay;

const MAX_ACTIVE_SESSIONS_V1: usize = 32;
const MAX_ACTIVITY_ROWS_PER_SESSION_V1: usize = 64;
const MAX_RECENT_OBSERVATIONS_V1: usize = 256;
const MAX_PROXIMITY_EVIDENCE_V1: usize = 32;
const ACTIVITY_HORIZON_SECONDS_V1: i64 = 300;
const EVIDENCE_TTL_MICROS_V1: i64 = 30_000_000;

type SessionKey = (String, String);

#[derive(Clone)]
struct StoredAgentObservation {
    sequence: u64,
    envelope: CanonicalObservationEnvelopeV1,
    anchor: tracedecay_domain::RetrievalAnchorId,
}

/// Owned production authority mounted by the PR13 registrar.
///
/// `sessions` is the already-open canonical project session/observation
/// database. `graph` is the already-open project graph for this exact
/// worktree. The authority performs reads only and owns no cache or store.
pub struct ProductionProximityEvidenceAuthorityV1 {
    sessions: Arc<GlobalDb>,
    graph: Arc<TraceDecay>,
    scope: FeedbackScopeV1,
    worktree_root: PathBuf,
    normalized_worktree: String,
}

pub type SharedCanonicalProximityEvidenceAuthorityV1 =
    Arc<dyn CanonicalProximityEvidenceAuthorityV1 + Send + Sync>;

impl ProductionProximityEvidenceAuthorityV1 {
    pub fn new(
        sessions: Arc<GlobalDb>,
        graph: Arc<TraceDecay>,
        scope: FeedbackScopeV1,
        worktree_root: PathBuf,
    ) -> Option<Self> {
        scope.validate().ok()?;
        let normalized_worktree = normalize_worktree(worktree_root.to_str()?);
        if normalized_worktree.is_empty()
            || normalize_worktree(graph.project_root().to_str()?) != normalized_worktree
        {
            return None;
        }
        Some(Self {
            sessions,
            graph,
            scope,
            worktree_root,
            normalized_worktree,
        })
    }

    async fn load(
        &self,
        request: &ProximityEvaluationRequestV1,
    ) -> Option<CanonicalProximityEvidenceBatchV1> {
        let observed_seconds = request.observed_at.0.div_euclid(1_000_000);
        let since = observed_seconds.saturating_sub(ACTIVITY_HORIZON_SECONDS_V1);
        let branch = request
            .scope
            .branch_ref
            .strip_prefix("refs/heads/")
            .unwrap_or(request.scope.branch_ref.as_str());
        let hits = self
            .sessions
            .git_sessions_for(&SessionsForQuery {
                git_ref: GitRefFilter::Worktree(self.normalized_worktree.clone()),
                since: Some(since),
                until: Some(observed_seconds),
                limit: MAX_ACTIVE_SESSIONS_V1,
            })
            .await
            .ok()?;
        let mut partial = hits.len() == MAX_ACTIVE_SESSIONS_V1;
        let mut active = BTreeMap::new();
        for hit in hits {
            if !hit_matches_scope(&hit, branch, &self.normalized_worktree) {
                continue;
            }
            let Some(session) = self
                .sessions
                .get_session(&hit.provider, &hit.session_id)
                .await
            else {
                partial = true;
                continue;
            };
            if normalize_worktree(&session.project_path) != self.normalized_worktree {
                partial = true;
                continue;
            }
            active.insert((hit.provider.clone(), hit.session_id.clone()), hit);
        }
        if active.len() < 2 {
            return CanonicalProximityEvidenceBatchV1::new(
                Vec::new(),
                if partial {
                    ProximityCoverageV1::Partial
                } else {
                    ProximityCoverageV1::Complete
                },
            );
        }

        let mut edits: BTreeMap<String, BTreeSet<SessionKey>> = BTreeMap::new();
        for key in active.keys() {
            let rows = self
                .sessions
                .session_messages_after(
                    key.0.as_str(),
                    key.1.as_str(),
                    since,
                    MAX_ACTIVITY_ROWS_PER_SESSION_V1,
                )
                .await
                .ok()?;
            partial |= rows.len() == MAX_ACTIVITY_ROWS_PER_SESSION_V1;
            for row in rows {
                for path in edited_paths(row.metadata_json.as_deref(), &self.worktree_root) {
                    edits.entry(path).or_default().insert(key.clone());
                }
            }
        }

        let observation_store = GlobalDbObservationStore::new(self.sessions.as_ref());
        let checkpoint = observation_store.projection_checkpoint().await.ok()?;
        let after_sequence = checkpoint
            .last_sequence()
            .saturating_sub(MAX_RECENT_OBSERVATIONS_V1 as u64);
        partial |= checkpoint.last_sequence() > MAX_RECENT_OBSERVATIONS_V1 as u64;
        let replay =
            ObservationReplayRequest::new(after_sequence, MAX_RECENT_OBSERVATIONS_V1).ok()?;
        let rows = observation_store.replay_observations(replay).await.ok()?;
        partial |= rows.len() == MAX_RECENT_OBSERVATIONS_V1;
        let project_scope = ObservationScopeV1::Project {
            project_id: request.scope.project_id.clone(),
        };
        let mut observations = BTreeMap::<SessionKey, StoredAgentObservation>::new();
        for row in rows {
            if row.observation().scope() != &project_scope {
                continue;
            }
            let Ok(envelope) = serde_json::from_value::<CanonicalObservationEnvelopeV1>(
                row.observation().payload().clone(),
            ) else {
                partial = true;
                continue;
            };
            if envelope.relations().agent_id().is_none() {
                continue;
            }
            let key = (
                envelope.provider().as_str().to_owned(),
                envelope.relations().session_id().as_str().to_owned(),
            );
            if !active.contains_key(&key) {
                continue;
            }
            let candidate = StoredAgentObservation {
                sequence: row.sequence(),
                envelope,
                anchor: row.retrieval_anchor_id().clone(),
            };
            if observations
                .get(&key)
                .is_none_or(|current| candidate.sequence > current.sequence)
            {
                observations.insert(key, candidate);
            }
        }
        partial |= active.keys().any(|key| !observations.contains_key(key));

        let mut overlapping = edits
            .into_iter()
            .filter(|(_, sessions)| sessions.len() >= 2)
            .collect::<Vec<_>>();
        partial |= overlapping.len() > MAX_PROXIMITY_EVIDENCE_V1;
        overlapping.truncate(MAX_PROXIMITY_EVIDENCE_V1);
        let expires_at = UtcMicros(request.observed_at.0.checked_add(EVIDENCE_TTL_MICROS_V1)?);
        let mut evidence = Vec::with_capacity(overlapping.len());
        for (path, session_keys) in overlapping {
            let selected = session_keys
                .iter()
                .filter_map(|key| observations.get(key))
                .collect::<Vec<_>>();
            let agents = selected
                .iter()
                .filter_map(|observation| observation.envelope.relations().agent_id())
                .map(|agent| agent.as_str())
                .collect::<BTreeSet<_>>();
            if selected.len() < 2 || agents.len() < 2 {
                partial = true;
                continue;
            }
            let graph_nodes = if let Ok(nodes) = self.graph.get_nodes_by_file(&path).await {
                nodes
            } else {
                partial = true;
                Vec::new()
            };
            let blast_radius_size = if graph_nodes.is_empty() {
                partial = true;
                1
            } else {
                let seeds = graph_nodes
                    .iter()
                    .map(|node| node.id.clone())
                    .collect::<Vec<_>>();
                if let Ok(nodes) = self.graph.get_impact_radius_multi(&seeds, 1).await {
                    u32::try_from(nodes.len().max(1)).unwrap_or(u32::MAX)
                } else {
                    partial = true;
                    u32::try_from(graph_nodes.len()).unwrap_or(u32::MAX)
                }
            };
            let latest_activity = session_keys
                .iter()
                .filter_map(|key| active.get(key))
                .filter_map(|hit| hit.last_ts.or(hit.committed_at).or(hit.first_ts))
                .max()
                .unwrap_or(since);
            let age = observed_seconds.saturating_sub(latest_activity).max(0);
            let freshness = 10_000_u16.saturating_sub(
                u16::try_from(
                    age.saturating_mul(10_000)
                        .div_euclid(ACTIVITY_HORIZON_SECONDS_V1)
                        .min(10_000),
                )
                .unwrap_or(10_000),
            );
            evidence.push(CanonicalProximityEvidenceV1 {
                observations: selected
                    .iter()
                    .map(|observation| observation.envelope.clone())
                    .collect(),
                retrieval_anchor_ids: selected
                    .iter()
                    .map(|observation| observation.anchor.clone())
                    .collect(),
                address: ProximityAddressV1 {
                    scope: request.scope.clone(),
                    file: FileOccurrenceId::new(path).ok()?,
                    span: None,
                    symbol: None,
                },
                relation_paths: Vec::new(),
                risk_inputs: ProximityRiskInputsV1 {
                    overlap_size: u32::try_from(selected.len()).unwrap_or(u32::MAX),
                    blast_radius_size,
                    relation_strength: ProximityRelationStrengthV1::Direct,
                    branch_worktree_incompatibility:
                        ProximityBranchWorktreeIncompatibilityV1::Compatible,
                    freshness_decay_basis_points: freshness,
                },
                warning_class: ProximityWarningClassV1::SameFile,
                raw_risk_basis_points: 10_000,
                observed_at: request.observed_at,
                expires_at,
                coverage: if partial {
                    ProximityCoverageV1::Partial
                } else {
                    ProximityCoverageV1::Complete
                },
            });
        }
        CanonicalProximityEvidenceBatchV1::new(
            evidence,
            if partial {
                ProximityCoverageV1::Partial
            } else {
                ProximityCoverageV1::Complete
            },
        )
    }
}

impl CanonicalProximityEvidenceAuthorityV1 for ProductionProximityEvidenceAuthorityV1 {
    fn current_evidence<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a ProximityEvaluationRequestV1,
    ) -> FeedbackPortFuture<'a, Option<CanonicalProximityEvidenceBatchV1>> {
        Box::pin(async move {
            if request.validate().is_err()
                || request.scope != self.scope
                || !super::super::context_allows_feedback_operation(
                    context,
                    &request.scope,
                    PROXIMITY_CAPABILITY_ID_V1,
                    PROXIMITY_USE_CASE_ID_V1,
                )
            {
                return None;
            }
            self.load(request).await
        })
    }
}

/// Constructor used by the PR13 registrar. Returning an owned trait-object
/// keeps the already-open project authorities alive without a new store.
pub fn production_proximity_evidence_authority_v1(
    sessions: Arc<GlobalDb>,
    graph: Arc<TraceDecay>,
    scope: FeedbackScopeV1,
    worktree_root: PathBuf,
) -> Option<SharedCanonicalProximityEvidenceAuthorityV1> {
    Some(Arc::new(ProductionProximityEvidenceAuthorityV1::new(
        sessions,
        graph,
        scope,
        worktree_root,
    )?))
}

fn hit_matches_scope(hit: &SessionGitCorrelationHit, branch: &str, worktree: &str) -> bool {
    hit.branch.as_deref() == Some(branch)
        && hit
            .worktree
            .as_deref()
            .is_some_and(|candidate| normalize_worktree(candidate) == worktree)
}

fn edited_paths(metadata: Option<&str>, worktree_root: &Path) -> Vec<String> {
    let Some(Value::Object(metadata)) = metadata.and_then(|value| serde_json::from_str(value).ok())
    else {
        return Vec::new();
    };
    let mut paths = metadata
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.get("path").and_then(Value::as_str))
        .chain(
            metadata
                .get("edited_file")
                .and_then(|value| value.get("path"))
                .and_then(Value::as_str),
        )
        .filter_map(|path| project_relative_path(worktree_root, path))
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

fn project_relative_path(worktree_root: &Path, value: &str) -> Option<String> {
    let normalized = normalize_worktree(value);
    if normalized.is_empty() || normalized.chars().any(char::is_control) {
        return None;
    }
    let root = normalize_worktree(worktree_root.to_str()?);
    let relative = normalized
        .strip_prefix(root.as_str())
        .and_then(|suffix| suffix.strip_prefix('/'))
        .unwrap_or(normalized.as_str())
        .trim_start_matches("./");
    if relative.is_empty()
        || relative
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return None;
    }
    Some(relative.to_owned())
}

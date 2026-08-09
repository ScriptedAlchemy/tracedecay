//! Production proximity evidence over existing read authorities.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::Value;
use tracedecay_application::RequestContext;
use tracedecay_application::feedback::{
    FeedbackPortFuture, PROXIMITY_CAPABILITY_ID_V1, PROXIMITY_USE_CASE_ID_V1,
    ProximityEvaluationRequestV1,
};
use tracedecay_code_index::graph_projection::{
    CodeGraphInteractiveReader, CodeGraphSymbolSummaryV1,
};
use tracedecay_domain::feedback::{
    FeedbackScopeV1, ProximityAddressV1, ProximityBranchWorktreeIncompatibilityV1,
    ProximityCoverageV1, ProximityRelationPathKindV1, ProximityRelationPathV1,
    ProximityRelationStrengthV1, ProximityRiskInputsV1, ProximityWarningClassV1,
};
use tracedecay_domain::{
    CanonicalObservationEnvelopeV1, ContentDigest, ObservationScopeV1, RelationEdgeKindV1,
    SourceSpan, SymbolOccurrenceId, UtcMicros,
};
use tracedecay_graph_db::{GraphNamespace, NeverCancelled};
use tracedecay_store::{ObservationProjectionStore, ObservationReplayRequest, ObservationStore};

use super::{
    CanonicalProximityEvidenceAuthorityV1, CanonicalProximityEvidenceBatchV1,
    CanonicalProximityEvidenceV1,
};
use crate::graph::{CodeGraphProjectionReadPort, CodeGraphReadRequest, request_graph_cancellation};
use crate::store::GlobalDbObservationStore;
use crate::tracedecay::TraceDecay;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_sessions::runtime::git_correlation::{
    GitEvidenceProjectionStore, GitRefFilter, SessionsForQuery, git_evidence_projection_identity,
    normalize_worktree,
};

const MAX_ACTIVE_SESSIONS_V1: usize = 32;
const MAX_ACTIVITY_ROWS_PER_SESSION_V1: usize = 64;
const MAX_RECENT_OBSERVATIONS_V1: usize = 256;
const MAX_PROXIMITY_EVIDENCE_V1: usize = 32;
const MAX_EDITED_PATHS_V1: usize = 64;
const ACTIVITY_HORIZON_SECONDS_V1: i64 = 300;
const EVIDENCE_TTL_MICROS_V1: i64 = 30_000_000;

type SessionKey = (String, String);

#[derive(Clone)]
struct StoredAgentObservation {
    sequence: u64,
    envelope: CanonicalObservationEnvelopeV1,
    anchor: tracedecay_domain::RetrievalAnchorId,
}

struct ProximityCandidate {
    path: String,
    session_keys: BTreeSet<SessionKey>,
    warning_class: ProximityWarningClassV1,
    relation_kinds: Vec<ProximityRelationPathKindV1>,
    relation_strength: ProximityRelationStrengthV1,
}

/// Owned production authority mounted by the advisory registrar.
///
/// `sessions` is the already-open canonical project session/observation
/// database. `graph` is the already-open project graph for this exact
/// worktree. The authority performs reads only and owns no cache or store.
pub struct ProductionProximityEvidenceAuthorityV1 {
    sessions: Arc<RegisteredGlobalDb>,
    code_graph: Arc<dyn CodeGraphProjectionReadPort>,
    scope: FeedbackScopeV1,
    worktree_root: PathBuf,
    normalized_worktree: String,
    code_index_identity:
        Option<Arc<dyn crate::diagnostics_publication::CodeIndexPublicationIdentityPortV1>>,
}

pub type SharedCanonicalProximityEvidenceAuthorityV1 =
    Arc<dyn CanonicalProximityEvidenceAuthorityV1 + Send + Sync>;

impl ProductionProximityEvidenceAuthorityV1 {
    pub(crate) fn new(
        sessions: Arc<RegisteredGlobalDb>,
        graph: Arc<TraceDecay>,
        code_graph: Arc<dyn CodeGraphProjectionReadPort>,
        scope: FeedbackScopeV1,
        _worktree_root: PathBuf,
    ) -> Option<Self> {
        scope.validate().ok()?;
        let worktree_root = graph.project_root().to_path_buf();
        let normalized_worktree = normalize_worktree(worktree_root.to_str()?);
        if normalized_worktree.is_empty() {
            return None;
        }
        if !matches!(
            &sessions.binding().shard_id.scope,
            tracedecay_store::StoreShardScopeV1::ProjectSessions { project_id }
                if project_id == &scope.project_id
        ) {
            return None;
        }
        Some(Self {
            sessions,
            code_graph,
            scope,
            worktree_root,
            normalized_worktree,
            code_index_identity: None,
        })
    }

    fn with_code_index_identity(
        mut self,
        code_index_identity: Arc<
            dyn crate::diagnostics_publication::CodeIndexPublicationIdentityPortV1,
        >,
    ) -> Self {
        self.code_index_identity = Some(code_index_identity);
        self
    }

    async fn load(
        &self,
        context: &RequestContext,
        request: &ProximityEvaluationRequestV1,
    ) -> Option<CanonicalProximityEvidenceBatchV1> {
        let cancellation = request_graph_cancellation(context);
        let verified = self
            .code_graph
            .open(CodeGraphReadRequest::new(
                context,
                request.observed_at,
                Arc::clone(&cancellation),
            ))
            .await
            .ok()?;
        let reader = verified
            .reader_with_cancellation(context, request.observed_at, Arc::clone(&cancellation))
            .ok()?;
        let code_index_identity = if let Some(resolver) = self.code_index_identity.as_ref() {
            let Some(identity) = resolver.resolve(self.worktree_root.clone()).await else {
                return Some(CanonicalProximityEvidenceBatchV1 {
                    evidence: Vec::new(),
                    coverage: ProximityCoverageV1::Partial,
                });
            };
            if identity.source_revision() != Some(&request.scope.head_commit_id) {
                return Some(CanonicalProximityEvidenceBatchV1 {
                    evidence: Vec::new(),
                    coverage: ProximityCoverageV1::Partial,
                });
            }
            Some(identity)
        } else {
            None
        };
        let observed_seconds = request.observed_at.0.div_euclid(1_000_000);
        let since = observed_seconds.saturating_sub(ACTIVITY_HORIZON_SECONDS_V1);
        // The legacy path column is only a bounded lookup hint. Exact identity
        // was already admitted by typed project/repository/worktree scope, and
        // saved-generation content is rechecked before publication.
        let projection =
            git_evidence_projection_identity(GraphNamespace::new("project").ok()?).ok()?;
        // A never-published projection carries no session evidence; fall back
        // to the same "no evidence" path as an unreadable one.
        let snapshot = self
            .sessions
            .project_graph_runtime()?
            .verified_snapshot(&projection, Arc::new(AtomicBool::new(false)))
            .ok()
            .flatten()?;
        let store =
            GitEvidenceProjectionStore::from_verified_snapshot(snapshot, Arc::new(NeverCancelled))
                .ok()?;
        let hits = store.sessions_for(&SessionsForQuery {
            git_ref: GitRefFilter::Worktree(self.normalized_worktree.clone()),
            since: Some(since),
            until: Some(observed_seconds),
            limit: MAX_ACTIVE_SESSIONS_V1,
        });
        let mut partial = hits.len() == MAX_ACTIVE_SESSIONS_V1;
        let mut active = BTreeMap::new();
        for hit in hits {
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
        let mut session_edits: BTreeMap<SessionKey, BTreeSet<String>> = BTreeMap::new();
        let mut session_edit_spans = BTreeMap::<(SessionKey, String), Vec<SourceSpan>>::new();
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
                for edit in edited_paths(row.metadata_json.as_deref(), &self.worktree_root) {
                    edits
                        .entry(edit.path.clone())
                        .or_default()
                        .insert(key.clone());
                    session_edits
                        .entry(key.clone())
                        .or_default()
                        .insert(edit.path.clone());
                    if let Some(span) = edit.span {
                        session_edit_spans
                            .entry((key.clone(), edit.path))
                            .or_default()
                            .push(span);
                    }
                }
            }
        }
        if edits.len() > MAX_EDITED_PATHS_V1 {
            partial = true;
            let retained = edits
                .keys()
                .take(MAX_EDITED_PATHS_V1)
                .cloned()
                .collect::<BTreeSet<_>>();
            edits.retain(|path, _| retained.contains(path));
            for paths in session_edits.values_mut() {
                paths.retain(|path| retained.contains(path));
            }
        }

        let observation_store = GlobalDbObservationStore::with_runtime(
            self.sessions.runtime(),
            self.sessions.authority(),
        );
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

        let mut graph_nodes = BTreeMap::<String, Vec<CodeGraphSymbolSummaryV1>>::new();
        let mut verified_graph_paths = BTreeMap::new();
        for path in edits.keys() {
            match reader.symbols_in_logical_file(path, 100_000, Arc::clone(&cancellation)) {
                Ok(nodes) => {
                    graph_nodes.insert(path.clone(), nodes);
                    let source = std::fs::read_to_string(self.worktree_root.join(path));
                    let record = reader.file_by_logical_path(path, Arc::clone(&cancellation));
                    match (source, record) {
                        (Ok(source), Ok(Some(record)))
                            if normalized_content_digest(
                                &tracedecay_runtime_core::sync::content_hash(&source),
                            ) == Some(record.content_digest.clone()) =>
                        {
                            verified_graph_paths.insert(
                                path.clone(),
                                (record.file_occurrence_id, record.content_digest),
                            );
                        }
                        _ => {
                            partial = true;
                        }
                    }
                }
                Err(_) => {
                    partial = true;
                }
            }
        }
        let mut candidates = edits
            .iter()
            .filter(|(_, sessions)| sessions.len() >= 2)
            .map(|(path, sessions)| ProximityCandidate {
                path: path.clone(),
                session_keys: sessions.clone(),
                warning_class: ProximityWarningClassV1::SameFile,
                relation_kinds: Vec::new(),
                relation_strength: ProximityRelationStrengthV1::Direct,
            })
            .collect::<Vec<_>>();
        let session_keys = session_edits.keys().cloned().collect::<Vec<_>>();
        let mut relation_cache = BTreeMap::<(String, String), (Option<GraphRelation>, bool)>::new();
        'session_pairs: for left_index in 0..session_keys.len() {
            for right_index in left_index.saturating_add(1)..session_keys.len() {
                let left_key = &session_keys[left_index];
                let right_key = &session_keys[right_index];
                let Some(left_paths) = session_edits.get(left_key) else {
                    continue;
                };
                let Some(right_paths) = session_edits.get(right_key) else {
                    continue;
                };
                for left_path in left_paths {
                    for right_path in right_paths {
                        if candidates.len() > MAX_PROXIMITY_EVIDENCE_V1 {
                            break 'session_pairs;
                        }
                        if left_path == right_path {
                            continue;
                        }
                        let path_pair = if left_path < right_path {
                            (left_path.clone(), right_path.clone())
                        } else {
                            (right_path.clone(), left_path.clone())
                        };
                        let (relation, relation_partial) =
                            if let Some(cached) = relation_cache.get(&path_pair) {
                                cached.clone()
                            } else {
                                let (Some(left_nodes), Some(right_nodes)) =
                                    (graph_nodes.get(&path_pair.0), graph_nodes.get(&path_pair.1))
                                else {
                                    partial = true;
                                    continue;
                                };
                                let resolved = graph_relation(
                                    &reader,
                                    Arc::clone(&cancellation),
                                    left_nodes,
                                    right_nodes,
                                );
                                relation_cache.insert(path_pair.clone(), resolved.clone());
                                resolved
                            };
                        partial |= relation_partial;
                        let Some(relation) = relation else {
                            continue;
                        };
                        candidates.push(ProximityCandidate {
                            path: path_pair.0,
                            session_keys: BTreeSet::from([left_key.clone(), right_key.clone()]),
                            warning_class: relation.warning_class,
                            relation_kinds: relation.kinds,
                            relation_strength: relation.strength,
                        });
                    }
                }
            }
        }
        candidates.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| {
                    proximity_warning_rank(left.warning_class)
                        .cmp(&proximity_warning_rank(right.warning_class))
                })
                .then_with(|| left.session_keys.cmp(&right.session_keys))
        });
        partial |= candidates.len() > MAX_PROXIMITY_EVIDENCE_V1;
        candidates.truncate(MAX_PROXIMITY_EVIDENCE_V1);
        let expires_at = UtcMicros(request.observed_at.0.checked_add(EVIDENCE_TTL_MICROS_V1)?);
        let mut evidence = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let file = if let Some(identity) = code_index_identity.as_ref() {
                let Some((file, indexed_digest)) = identity.file(&candidate.path) else {
                    partial = true;
                    continue;
                };
                let Some((_, graph_digest)) = verified_graph_paths.get(&candidate.path) else {
                    partial = true;
                    continue;
                };
                if indexed_digest != graph_digest {
                    partial = true;
                    continue;
                }
                file.clone()
            } else {
                let Some((file, _)) = verified_graph_paths.get(&candidate.path) else {
                    partial = true;
                    continue;
                };
                file.clone()
            };
            let selected = candidate
                .session_keys
                .iter()
                .filter_map(|key| observations.get(key))
                .collect::<Vec<_>>();
            let agents = selected
                .iter()
                .filter_map(|observation| observation.envelope.relations().agent_id())
                .map(tracedecay_domain::ObservationId::as_str)
                .collect::<BTreeSet<_>>();
            if selected.len() < 2 || agents.len() < 2 {
                partial = true;
                continue;
            }
            let path_nodes = graph_nodes
                .get(&candidate.path)
                .map_or(&[][..], Vec::as_slice);
            let blast_radius_size = if path_nodes.is_empty() {
                partial = true;
                1
            } else {
                let seeds = path_nodes
                    .iter()
                    .map(|node| node.occurrence.clone())
                    .collect::<Vec<_>>();
                if let Ok(nodes) =
                    reader.impact(&seeds, &[], 1, 100_000, 100_000, Arc::clone(&cancellation))
                {
                    u32::try_from(nodes.impacted.len().max(1)).unwrap_or(u32::MAX)
                } else {
                    partial = true;
                    u32::try_from(path_nodes.len()).unwrap_or(u32::MAX)
                }
            };
            let latest_activity = candidate
                .session_keys
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
            let exact_address = verified_graph_paths
                .contains_key(&candidate.path)
                .then(|| {
                    let ranges = candidate
                        .session_keys
                        .iter()
                        .map(|key| {
                            session_edit_spans
                                .get(&(key.clone(), candidate.path.clone()))
                                .map(Vec::as_slice)
                        })
                        .collect::<Option<Vec<_>>>()?;
                    exact_graph_address(&self.worktree_root, &candidate.path, path_nodes, &ranges)
                })
                .flatten();
            if candidate.warning_class == ProximityWarningClassV1::SameFile
                && !path_nodes.is_empty()
                && exact_address.is_none()
            {
                partial = true;
            }
            let warning_class =
                exact_warning_class(candidate.warning_class, exact_address.is_some());
            let relation_anchor = selected
                .first()
                .map(|observation| observation.anchor.clone());
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
                    file,
                    span: exact_address.as_ref().map(|address| address.0),
                    symbol: exact_address.map(|address| address.1),
                },
                relation_paths: candidate
                    .relation_kinds
                    .into_iter()
                    .map(|kind| ProximityRelationPathV1 {
                        kind,
                        retrieval_anchor_id: relation_anchor.clone(),
                    })
                    .collect(),
                risk_inputs: ProximityRiskInputsV1 {
                    overlap_size: u32::try_from(selected.len()).unwrap_or(u32::MAX),
                    blast_radius_size,
                    relation_strength: candidate.relation_strength,
                    branch_worktree_incompatibility:
                        ProximityBranchWorktreeIncompatibilityV1::Compatible,
                    freshness_decay_basis_points: freshness,
                },
                warning_class,
                raw_risk_basis_points: if matches!(
                    warning_class,
                    ProximityWarningClassV1::SameFile
                        | ProximityWarningClassV1::OverlappingRange
                        | ProximityWarningClassV1::SameSymbol
                ) {
                    10_000
                } else {
                    7_500
                },
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
            self.load(context, request).await
        })
    }
}

/// Constructor used by the advisory registrar. Returning an owned trait-object
/// keeps the already-open project authorities alive without a new store.
pub(crate) fn production_proximity_evidence_authority_v1(
    sessions: Arc<RegisteredGlobalDb>,
    graph: Arc<TraceDecay>,
    code_graph: Arc<dyn CodeGraphProjectionReadPort>,
    scope: FeedbackScopeV1,
    worktree_root: PathBuf,
    code_index_identity: Arc<
        dyn crate::diagnostics_publication::CodeIndexPublicationIdentityPortV1,
    >,
) -> Option<SharedCanonicalProximityEvidenceAuthorityV1> {
    Some(Arc::new(
        ProductionProximityEvidenceAuthorityV1::new(
            sessions,
            graph,
            code_graph,
            scope,
            worktree_root,
        )?
        .with_code_index_identity(code_index_identity),
    ))
}

#[derive(Clone)]
struct GraphRelation {
    warning_class: ProximityWarningClassV1,
    kinds: Vec<ProximityRelationPathKindV1>,
    strength: ProximityRelationStrengthV1,
}

fn graph_relation(
    graph: &CodeGraphInteractiveReader,
    cancellation: Arc<dyn tracedecay_graph_db::GraphCancellation>,
    left_nodes: &[CodeGraphSymbolSummaryV1],
    right_nodes: &[CodeGraphSymbolSummaryV1],
) -> (Option<GraphRelation>, bool) {
    let left_ids = left_nodes
        .iter()
        .map(|node| node.occurrence.clone())
        .collect::<BTreeSet<_>>();
    let right_ids = right_nodes
        .iter()
        .map(|node| node.occurrence.clone())
        .collect::<BTreeSet<_>>();
    if left_ids.iter().any(|node_id| right_ids.contains(node_id)) {
        return (
            Some(GraphRelation {
                warning_class: ProximityWarningClassV1::SameSymbol,
                kinds: Vec::new(),
                strength: ProximityRelationStrengthV1::Direct,
            }),
            false,
        );
    }

    let left_seeds = left_ids.iter().take(32).cloned().collect::<Vec<_>>();
    let right_seeds = right_ids.iter().take(32).cloned().collect::<Vec<_>>();
    let Ok(left_edges) = graph.callees(
        &left_seeds,
        &[RelationEdgeKindV1::Calls],
        100_000,
        Arc::clone(&cancellation),
    ) else {
        return (None, true);
    };
    for edges in left_edges {
        if edges
            .iter()
            .any(|edge| right_ids.contains(&edge.edge.to_occurrence))
        {
            return (
                Some(GraphRelation {
                    warning_class: ProximityWarningClassV1::Neighborhood,
                    kinds: vec![ProximityRelationPathKindV1::DirectDependency],
                    strength: ProximityRelationStrengthV1::Direct,
                }),
                false,
            );
        }
    }
    let Ok(right_edges) = graph.callees(
        &right_seeds,
        &[RelationEdgeKindV1::Calls],
        100_000,
        Arc::clone(&cancellation),
    ) else {
        return (None, true);
    };
    for edges in right_edges {
        if edges
            .iter()
            .any(|edge| left_ids.contains(&edge.edge.to_occurrence))
        {
            return (
                Some(GraphRelation {
                    warning_class: ProximityWarningClassV1::Neighborhood,
                    kinds: vec![ProximityRelationPathKindV1::DirectCaller],
                    strength: ProximityRelationStrengthV1::Direct,
                }),
                false,
            );
        }
    }

    let Some((left_callers, left_dependencies, left_tests)) =
        graph_neighborhood(graph, Arc::clone(&cancellation), &left_seeds)
    else {
        return (None, true);
    };
    let Some((right_callers, right_dependencies, right_tests)) =
        graph_neighborhood(graph, cancellation, &right_seeds)
    else {
        return (None, true);
    };
    if !left_callers.is_disjoint(&right_callers) {
        return (
            Some(GraphRelation {
                warning_class: ProximityWarningClassV1::SharedCaller,
                kinds: vec![ProximityRelationPathKindV1::TransitiveCaller],
                strength: ProximityRelationStrengthV1::Transitive,
            }),
            false,
        );
    }
    if !left_dependencies.is_disjoint(&right_dependencies) {
        return (
            Some(GraphRelation {
                warning_class: ProximityWarningClassV1::SharedDependency,
                kinds: vec![ProximityRelationPathKindV1::TransitiveDependency],
                strength: ProximityRelationStrengthV1::Transitive,
            }),
            false,
        );
    }
    if !left_tests.is_disjoint(&right_tests) {
        return (
            Some(GraphRelation {
                warning_class: ProximityWarningClassV1::SharedTest,
                kinds: vec![ProximityRelationPathKindV1::AffectedTest],
                strength: ProximityRelationStrengthV1::Transitive,
            }),
            false,
        );
    }
    (None, false)
}

fn graph_neighborhood(
    graph: &CodeGraphInteractiveReader,
    cancellation: Arc<dyn tracedecay_graph_db::GraphCancellation>,
    seeds: &[SymbolOccurrenceId],
) -> Option<(
    BTreeSet<SymbolOccurrenceId>,
    BTreeSet<SymbolOccurrenceId>,
    BTreeSet<SymbolOccurrenceId>,
)> {
    let impact = graph
        .impact(
            seeds,
            &[RelationEdgeKindV1::Calls],
            3,
            100_000,
            100_000,
            Arc::clone(&cancellation),
        )
        .ok()?;
    let callers = impact
        .impacted
        .iter()
        .filter(|symbol| symbol.depth <= 2)
        .map(|symbol| symbol.summary.occurrence.clone())
        .collect();
    let tests = impact
        .impacted
        .iter()
        .filter(|symbol| {
            symbol
                .summary
                .metadata
                .as_ref()
                .is_some_and(|metadata| metadata.kind.eq_ignore_ascii_case("test"))
        })
        .map(|symbol| symbol.summary.occurrence.clone())
        .collect();
    let mut dependencies = BTreeSet::new();
    let mut frontier = seeds.to_vec();
    for _ in 0..2 {
        let edges = graph
            .callees(
                &frontier,
                &[RelationEdgeKindV1::Calls],
                100_000,
                Arc::clone(&cancellation),
            )
            .ok()?;
        frontier = edges
            .into_iter()
            .flatten()
            .map(|edge| edge.edge.to_occurrence)
            .filter(|occurrence| dependencies.insert(occurrence.clone()))
            .collect();
        if frontier.is_empty() {
            break;
        }
    }
    Some((callers, dependencies, tests))
}

fn exact_graph_address(
    worktree_root: &Path,
    path: &str,
    nodes: &[CodeGraphSymbolSummaryV1],
    session_ranges: &[&[SourceSpan]],
) -> Option<(SourceSpan, SymbolOccurrenceId)> {
    if session_ranges.len() < 2 {
        return None;
    }
    let _ = (worktree_root, path);
    let mut resolved_symbol = None::<(&CodeGraphSymbolSummaryV1, SourceSpan)>;
    for ranges in session_ranges {
        if ranges.is_empty() {
            return None;
        }
        let mut session_symbol = None::<(&CodeGraphSymbolSummaryV1, SourceSpan)>;
        for range in *ranges {
            if range.validate().is_err() || range.start_byte == range.end_byte {
                return None;
            }
            let candidate = resolve_edit_range_symbol(range, nodes)?;
            match &session_symbol {
                Some((current, _)) if current.occurrence != candidate.0.occurrence => return None,
                None => session_symbol = Some(candidate),
                _ => {}
            }
        }
        let session_symbol = session_symbol?;
        match &resolved_symbol {
            Some((current, _)) if current.occurrence != session_symbol.0.occurrence => return None,
            None => resolved_symbol = Some(session_symbol),
            _ => {}
        }
    }
    let (candidate, span) = resolved_symbol?;
    Some((span, candidate.occurrence.clone()))
}

const fn exact_warning_class(
    candidate: ProximityWarningClassV1,
    has_exact_current_symbol: bool,
) -> ProximityWarningClassV1 {
    match (candidate, has_exact_current_symbol) {
        (ProximityWarningClassV1::SameFile, true) => ProximityWarningClassV1::SameSymbol,
        (ProximityWarningClassV1::SameSymbol, false) => ProximityWarningClassV1::SameFile,
        _ => candidate,
    }
}

fn resolve_edit_range_symbol<'a>(
    edit_range: &SourceSpan,
    nodes: &'a [CodeGraphSymbolSummaryV1],
) -> Option<(&'a CodeGraphSymbolSummaryV1, SourceSpan)> {
    let mut candidates = nodes
        .iter()
        .filter_map(|node| {
            let span = node.binding.as_ref()?.source_span?;
            (span.start_byte <= edit_range.start_byte && span.end_byte >= edit_range.end_byte)
                .then_some((node, span))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.1
            .end_byte
            .saturating_sub(left.1.start_byte)
            .cmp(&right.1.end_byte.saturating_sub(right.1.start_byte))
            .then_with(|| left.0.occurrence.cmp(&right.0.occurrence))
    });
    let candidate = *candidates.first()?;
    let candidate_size = candidate.1.end_byte.saturating_sub(candidate.1.start_byte);
    if candidates
        .get(1)
        .is_some_and(|next| next.1.end_byte.saturating_sub(next.1.start_byte) == candidate_size)
    {
        return None;
    }
    Some(candidate)
}

fn normalized_content_digest(value: &str) -> Option<ContentDigest> {
    if value.starts_with("sha256:") {
        ContentDigest::new(value.to_owned()).ok()
    } else {
        ContentDigest::new(format!("sha256:{value}")).ok()
    }
}

const fn proximity_warning_rank(warning: ProximityWarningClassV1) -> u8 {
    match warning {
        ProximityWarningClassV1::SameSymbol => 0,
        ProximityWarningClassV1::OverlappingRange => 1,
        ProximityWarningClassV1::SameFile => 2,
        ProximityWarningClassV1::SharedCaller => 3,
        ProximityWarningClassV1::SharedDependency => 4,
        ProximityWarningClassV1::SharedTest => 5,
        ProximityWarningClassV1::SamePackage => 6,
        ProximityWarningClassV1::SameCrate => 7,
        ProximityWarningClassV1::Neighborhood => 8,
        ProximityWarningClassV1::IncompatibleBranchWorktree => 9,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct EditedPathEvidence {
    path: String,
    span: Option<SourceSpan>,
}

fn edited_paths(metadata: Option<&str>, worktree_root: &Path) -> Vec<EditedPathEvidence> {
    let Some(Value::Object(metadata)) = metadata.and_then(|value| serde_json::from_str(value).ok())
    else {
        return Vec::new();
    };
    let mut paths = metadata
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| edited_path_evidence(entry, worktree_root))
        .chain(
            metadata
                .get("edited_file")
                .and_then(|value| edited_path_evidence(value, worktree_root)),
        )
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

fn edited_path_evidence(value: &Value, worktree_root: &Path) -> Option<EditedPathEvidence> {
    let path = project_relative_path(worktree_root, value.get("path")?.as_str()?)?;
    let span = value
        .get("span")
        .or_else(|| value.get("range"))
        .and_then(|span| serde_json::from_value::<SourceSpan>(span.clone()).ok())
        .filter(|span| span.validate().is_ok() && span.start_byte < span.end_byte);
    Some(EditedPathEvidence { path, span })
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

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_code_index::graph_projection::CodeGraphSymbolBindingV1;
    use tracedecay_domain::{FileOccurrenceId, LanguageDescriptorRevision};

    fn callable(id: &str, span: SourceSpan) -> CodeGraphSymbolSummaryV1 {
        CodeGraphSymbolSummaryV1 {
            occurrence: SymbolOccurrenceId::new(id).unwrap(),
            binding: Some(CodeGraphSymbolBindingV1 {
                file: FileOccurrenceId::new("file.proximity").unwrap(),
                logical_path: Some("src/lib.rs".to_owned()),
                source_span: Some(span),
                chunk: None,
                language_descriptor_revision: LanguageDescriptorRevision::new("language.test")
                    .unwrap(),
            }),
            metadata: None,
        }
    }

    #[test]
    fn same_symbol_requires_each_typed_edit_range_to_resolve_to_that_symbol() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(
            root.path().join("src/lib.rs"),
            "fn alpha() {}\nfn beta() {}\n",
        )
        .unwrap();
        let nodes = vec![
            callable(
                "symbol.alpha",
                SourceSpan {
                    start_byte: 0,
                    end_byte: 14,
                },
            ),
            callable(
                "symbol.beta",
                SourceSpan {
                    start_byte: 14,
                    end_byte: 27,
                },
            ),
        ];
        let alpha = SourceSpan {
            start_byte: 3,
            end_byte: 8,
        };
        let beta = SourceSpan {
            start_byte: 17,
            end_byte: 21,
        };

        let same = exact_graph_address(
            root.path(),
            "src/lib.rs",
            &nodes,
            &[std::slice::from_ref(&alpha), std::slice::from_ref(&alpha)],
        )
        .unwrap();
        assert_eq!(same.1.as_str(), "symbol.alpha");
        assert!(
            exact_graph_address(
                root.path(),
                "src/lib.rs",
                &nodes,
                &[std::slice::from_ref(&alpha), std::slice::from_ref(&beta)],
            )
            .is_none()
        );
        assert!(exact_graph_address(root.path(), "src/lib.rs", &nodes, &[&[], &[]]).is_none());
        assert_eq!(
            exact_warning_class(ProximityWarningClassV1::SameFile, true),
            ProximityWarningClassV1::SameSymbol
        );
        assert_eq!(
            exact_warning_class(ProximityWarningClassV1::SameSymbol, false),
            ProximityWarningClassV1::SameFile
        );
    }

    #[test]
    fn edited_path_evidence_accepts_only_valid_typed_ranges() {
        let root = Path::new("/repo");
        let edits = edited_paths(
            Some(
                r#"{"files":[
                    {"path":"/repo/src/lib.rs","span":{"start_byte":4,"end_byte":9}},
                    {"path":"/repo/src/other.rs","span":{"start_byte":9,"end_byte":4}}
                ]}"#,
            ),
            root,
        );
        assert_eq!(edits.len(), 2);
        assert_eq!(
            edits
                .iter()
                .find(|edit| edit.path == "src/lib.rs")
                .and_then(|edit| edit.span),
            Some(SourceSpan {
                start_byte: 4,
                end_byte: 9
            })
        );
        assert!(
            edits
                .iter()
                .find(|edit| edit.path == "src/other.rs")
                .is_some_and(|edit| edit.span.is_none())
        );
    }
}

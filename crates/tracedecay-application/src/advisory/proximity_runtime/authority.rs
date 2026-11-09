//! Production proximity evidence over existing read authorities.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use tracedecay_code_index::graph_projection::{
    CodeGraphInteractiveReader, CodeGraphSymbolSummaryV1,
};
use tracedecay_contracts::RequestContext;
use tracedecay_contracts::feedback::{
    FeedbackPortFuture, FeedbackProximityAccessKindV1, FeedbackProximityEncounterV1,
    FeedbackProximityIntervalV1, FeedbackProximityOmissionV1, FeedbackProximityParticipantV1,
    FeedbackProximityRelationV1, PROXIMITY_CAPABILITY_ID_V1, PROXIMITY_USE_CASE_ID_V1,
    ProximityEvaluationRequestV1,
};
use tracedecay_domain::feedback::{
    FeedbackScopeV1, ProximityAddressV1, ProximityCoverageV1, ProximityRelationPathKindV1,
    ProximityRelationPathV1, ProximityWarningClassV1,
};
use tracedecay_domain::{
    AgentInstanceId, CanonicalObservationEnvelopeV1, CommitId, ContentDigest, FileOccurrenceId,
    ObservationScopeV1, ObservationSourceIdentityV1, RefId, RelationEdgeKindV1, SourceSpan,
    SymbolOccurrenceId, UtcMicros, canonical_sha256,
};
use tracedecay_store::{ObservationProjectionStore, ObservationReplayRequest, ObservationStore};

use super::{
    CanonicalProximityEvidenceAuthorityV1, CanonicalProximityEvidenceBatchV1,
    CanonicalProximityEvidenceV1, FeedbackProximityReadRuntimeV1,
};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_graph_query::{
    CodeGraphProjectionReadPort, CodeGraphReadRequest, request_graph_cancellation,
};
use tracedecay_sessions::runtime::git_correlation::{
    CommitRelationFilter, GitRefFilter, SessionGitCorrelationHit, SessionsForQuery,
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
    session_paths: BTreeMap<SessionKey, String>,
    warning_class: ProximityWarningClassV1,
    relation_kinds: Vec<ProximityRelationPathKindV1>,
}

type VerifiedGraphPaths = BTreeMap<String, (FileOccurrenceId, ContentDigest)>;

struct ProximityAssembly<'a> {
    normalized_worktree: &'a str,
    request: &'a ProximityEvaluationRequestV1,
    code_index_identity: Option<&'a crate::diagnostics_publication::CodeIndexPublicationIdentityV1>,
    verified_graph_paths: &'a VerifiedGraphPaths,
    observations: &'a BTreeMap<SessionKey, StoredAgentObservation>,
    graph_nodes: &'a BTreeMap<String, Vec<CodeGraphSymbolSummaryV1>>,
    session_edit_spans: &'a BTreeMap<(SessionKey, String), Vec<SourceSpan>>,
    active: &'a BTreeMap<SessionKey, SessionGitCorrelationHit>,
    expires_at: UtcMicros,
}

impl ProximityAssembly<'_> {
    fn evidence_for(
        &self,
        candidate: ProximityCandidate,
        omissions: &mut BTreeSet<FeedbackProximityOmissionV1>,
    ) -> Option<CanonicalProximityEvidenceV1> {
        let file = if let Some(identity) = self.code_index_identity {
            let Some((file, indexed_digest)) = identity.file(&candidate.path) else {
                omissions.insert(FeedbackProximityOmissionV1::MissingCodeAddress);
                return None;
            };
            let Some((_, graph_digest)) = self.verified_graph_paths.get(&candidate.path) else {
                omissions.insert(FeedbackProximityOmissionV1::MissingCodeAddress);
                return None;
            };
            if indexed_digest != graph_digest {
                omissions.insert(FeedbackProximityOmissionV1::CodeIndexRevisionMismatch);
                return None;
            }
            file.clone()
        } else {
            let Some((file, _)) = self.verified_graph_paths.get(&candidate.path) else {
                omissions.insert(FeedbackProximityOmissionV1::MissingCodeAddress);
                return None;
            };
            file.clone()
        };
        let selected = candidate
            .session_keys
            .iter()
            .filter_map(|key| {
                self.observations
                    .get(key)
                    .map(|observation| (key, observation))
            })
            .collect::<Vec<_>>();
        let agents = selected
            .iter()
            .filter_map(|(_, observation)| observation.envelope.relations().agent_id())
            .map(tracedecay_domain::ObservationId::as_str)
            .collect::<BTreeSet<_>>();
        if selected.len() < 2 || agents.len() < 2 {
            omissions.insert(FeedbackProximityOmissionV1::MissingParticipantObservation);
            return None;
        }
        let path_nodes = self
            .graph_nodes
            .get(&candidate.path)
            .map_or(&[][..], Vec::as_slice);
        if path_nodes.is_empty() {
            omissions.insert(FeedbackProximityOmissionV1::MissingCodeAddress);
        }
        let exact_address = self
            .verified_graph_paths
            .contains_key(&candidate.path)
            .then(|| {
                let ranges = candidate
                    .session_keys
                    .iter()
                    .map(|key| {
                        self.session_edit_spans
                            .get(&(key.clone(), candidate.path.clone()))
                            .map(Vec::as_slice)
                    })
                    .collect::<Option<Vec<_>>>()?;
                exact_graph_address(path_nodes, &ranges)
            })
            .flatten();
        if candidate.warning_class == ProximityWarningClassV1::SameFile
            && !path_nodes.is_empty()
            && exact_address.is_none()
        {
            omissions.insert(FeedbackProximityOmissionV1::MissingCodeAddress);
        }
        let warning_class = exact_warning_class(candidate.warning_class, exact_address.is_some());
        let relation_anchor = selected
            .first()
            .map(|(_, observation)| observation.anchor.clone());
        let mut participants = Vec::with_capacity(selected.len());
        for (key, observation) in &selected {
            let Some(hit) = self.active.get(*key) else {
                omissions.insert(FeedbackProximityOmissionV1::MissingParticipantObservation);
                continue;
            };
            let Some(worktree_root) = hit.worktree.clone() else {
                omissions.insert(FeedbackProximityOmissionV1::MissingParticipantWorktree);
                continue;
            };
            let Some(path) = candidate.session_paths.get(*key) else {
                omissions.insert(FeedbackProximityOmissionV1::MissingCodeAddress);
                continue;
            };
            let Some((participant_file, graph_digest)) = self.verified_graph_paths.get(path) else {
                omissions.insert(FeedbackProximityOmissionV1::MissingCodeAddress);
                continue;
            };
            if self
                .code_index_identity
                .and_then(|identity| identity.file(path))
                .is_some_and(|(_, indexed_digest)| indexed_digest != graph_digest)
            {
                omissions.insert(FeedbackProximityOmissionV1::CodeIndexRevisionMismatch);
                continue;
            }
            let participant_nodes = self.graph_nodes.get(path).map_or(&[][..], Vec::as_slice);
            let ranges = self
                .session_edit_spans
                .get(&((*key).clone(), path.clone()))
                .map_or_default(Vec::as_slice);
            let (span, symbol) = participant_graph_address(participant_nodes, ranges);
            let source = ObservationSourceIdentityV1::for_provider(
                observation.envelope.provider().clone(),
                observation.envelope.relations().session_id().clone(),
            )
            .ok()?;
            let agent_id = AgentInstanceId::new(
                observation
                    .envelope
                    .relations()
                    .agent_id()?
                    .as_str()
                    .to_owned(),
            )
            .ok()?;
            let activity = activity_interval(hit)?;
            let normalized = normalize_worktree(&worktree_root);
            let head_revision = if normalized == self.normalized_worktree {
                Some(self.request.scope.head_commit_id.clone())
            } else {
                hit.commit_sha
                    .as_ref()
                    .and_then(|commit| CommitId::new(commit.clone()).ok())
            };
            if head_revision.is_none() {
                omissions.insert(FeedbackProximityOmissionV1::MissingParticipantRevision);
            }
            participants.push(FeedbackProximityParticipantV1 {
                source,
                agent_id,
                worktree_id: (normalized == self.normalized_worktree)
                    .then(|| self.request.scope.worktree_id.clone()),
                worktree_root,
                branch_ref: hit.branch.as_deref().and_then(branch_ref),
                head_revision,
                access: FeedbackProximityAccessKindV1::Write,
                activity,
                address: ProximityAddressV1 {
                    scope: self.request.scope.clone(),
                    file: participant_file.clone(),
                    span,
                    symbol,
                },
            });
        }
        if participants.len() != 2 {
            return None;
        }
        let interval = FeedbackProximityIntervalV1 {
            start: participants
                .iter()
                .map(|participant| participant.activity.start)
                .max()?,
            end: participants
                .iter()
                .map(|participant| participant.activity.end)
                .min()?,
        };
        if interval.start.0 > interval.end.0 {
            return None;
        }
        let relation = if matches!(
            warning_class,
            ProximityWarningClassV1::SameFile
                | ProximityWarningClassV1::OverlappingRange
                | ProximityWarningClassV1::SameSymbol
        ) {
            FeedbackProximityRelationV1::OverlappingEdit { warning_class }
        } else {
            FeedbackProximityRelationV1::CodeNeighborhoodCandidate { warning_class }
        };
        let encounter_id = canonical_sha256(&(
            "tracedecay.feedback.proximity.encounter.v1",
            &self.request.scope,
            &interval,
            &participants,
            &relation,
            self.request.observed_at,
            self.expires_at,
        ))
        .ok()?;
        let encounter = FeedbackProximityEncounterV1 {
            encounter_id,
            scope: self.request.scope.clone(),
            interval,
            participants,
            relation,
            observed_at: self.request.observed_at,
            expires_at: self.expires_at,
            coverage: ProximityCoverageV1::Complete,
        };
        Some(CanonicalProximityEvidenceV1 {
            observations: selected
                .iter()
                .map(|(_, observation)| observation.envelope.clone())
                .collect(),
            retrieval_anchor_ids: selected
                .iter()
                .map(|(_, observation)| observation.anchor.clone())
                .collect(),
            encounter,
            address: ProximityAddressV1 {
                scope: self.request.scope.clone(),
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
            warning_class,
            observed_at: self.request.observed_at,
            expires_at: self.expires_at,
            coverage: ProximityCoverageV1::Complete,
        })
    }
}

/// Owned production authority mounted by the advisory registrar.
pub struct ProductionProximityEvidenceAuthorityV1 {
    sessions: RegisteredGlobalDbLeaseV1,
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
        sessions: RegisteredGlobalDbLeaseV1,
        code_graph: Arc<dyn CodeGraphProjectionReadPort>,
        scope: FeedbackScopeV1,
        worktree_root: PathBuf,
    ) -> Option<Self> {
        scope.validate().ok()?;
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
        let source_generation = reader.generation().clone();
        let expires_at = UtcMicros(request.observed_at.0.checked_add(EVIDENCE_TTL_MICROS_V1)?);
        let mut omissions = BTreeSet::new();
        let code_index_identity = if let Some(resolver) = self.code_index_identity.as_ref() {
            let Some(identity) = resolver.resolve(self.worktree_root.clone()).await else {
                return Some(CanonicalProximityEvidenceBatchV1 {
                    evidence: Vec::new(),
                    coverage: ProximityCoverageV1::Partial,
                    source_generation,
                    observed_at: request.observed_at,
                    expires_at,
                    omissions: vec![FeedbackProximityOmissionV1::CodeIndexRevisionMismatch],
                });
            };
            if identity.source_revision() != Some(&request.scope.head_commit_id) {
                return Some(CanonicalProximityEvidenceBatchV1 {
                    evidence: Vec::new(),
                    coverage: ProximityCoverageV1::Partial,
                    source_generation,
                    observed_at: request.observed_at,
                    expires_at,
                    omissions: vec![FeedbackProximityOmissionV1::CodeIndexRevisionMismatch],
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
        // Unrecorded or unreadable Git evidence yields no proximity evidence.
        // Branch scope retains separately registered worktrees.
        let sessions =
            tracedecay_global_db::GlobalDbGitCorrelationStore::new(self.sessions.clone());
        let (hits, presence) = sessions
            .sessions_for_with_relation_and_presence(
                &SessionsForQuery {
                    git_ref: GitRefFilter::Branch(
                        request
                            .scope
                            .branch_ref
                            .strip_prefix("refs/heads/")
                            .unwrap_or(request.scope.branch_ref.as_str())
                            .to_owned(),
                    ),
                    since: Some(since),
                    until: Some(observed_seconds),
                    limit: MAX_ACTIVE_SESSIONS_V1,
                },
                CommitRelationFilter::Produced,
            )
            .await
            .ok()?;
        if !presence.projection_available {
            return None;
        }
        if hits.len() == MAX_ACTIVE_SESSIONS_V1 {
            omissions.insert(FeedbackProximityOmissionV1::ActiveSessionLimit);
        }
        let mut active = BTreeMap::new();
        for hit in hits {
            active.insert((hit.provider.clone(), hit.session_id.clone()), hit);
        }
        if active.len() < 2 {
            return CanonicalProximityEvidenceBatchV1 {
                evidence: Vec::new(),
                coverage: if omissions.is_empty() {
                    ProximityCoverageV1::Complete
                } else {
                    ProximityCoverageV1::Partial
                },
                source_generation,
                observed_at: request.observed_at,
                expires_at,
                omissions: omissions.into_iter().collect(),
            }
            .validated();
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
            if rows.len() == MAX_ACTIVITY_ROWS_PER_SESSION_V1 {
                omissions.insert(FeedbackProximityOmissionV1::SessionActivityLimit);
            }
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
            omissions.insert(FeedbackProximityOmissionV1::EditedPathLimit);
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

        let observation_store = self.sessions.observation_store();
        let checkpoint = observation_store.projection_checkpoint().await.ok()?;
        let after_sequence = checkpoint
            .last_sequence()
            .saturating_sub(MAX_RECENT_OBSERVATIONS_V1 as u64);
        if checkpoint.last_sequence() > MAX_RECENT_OBSERVATIONS_V1 as u64 {
            omissions.insert(FeedbackProximityOmissionV1::RecentObservationLimit);
        }
        let replay =
            ObservationReplayRequest::new(after_sequence, MAX_RECENT_OBSERVATIONS_V1).ok()?;
        let rows = observation_store.replay_observations(replay).await.ok()?;
        if rows.len() == MAX_RECENT_OBSERVATIONS_V1 {
            omissions.insert(FeedbackProximityOmissionV1::RecentObservationLimit);
        }
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
                omissions.insert(FeedbackProximityOmissionV1::MissingParticipantObservation);
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
        if active.keys().any(|key| !observations.contains_key(key)) {
            omissions.insert(FeedbackProximityOmissionV1::MissingParticipantObservation);
        }

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
                            if ContentDigest::of_bytes(source.as_bytes())
                                == record.content_digest =>
                        {
                            verified_graph_paths.insert(
                                path.clone(),
                                (record.file_occurrence_id, record.content_digest),
                            );
                        }
                        _ => {
                            omissions.insert(FeedbackProximityOmissionV1::MissingCodeAddress);
                        }
                    }
                }
                Err(_) => {
                    omissions.insert(FeedbackProximityOmissionV1::MissingCodeAddress);
                }
            }
        }
        let mut candidates = edits
            .iter()
            .filter(|(_, sessions)| sessions.len() >= 2)
            .flat_map(|(path, sessions)| {
                let sessions = sessions.iter().cloned().collect::<Vec<_>>();
                (0..sessions.len()).flat_map(move |left| {
                    let sessions = sessions.clone();
                    let path = path.clone();
                    (left.saturating_add(1)..sessions.len()).map(move |right| {
                        let left = sessions[left].clone();
                        let right = sessions[right].clone();
                        ProximityCandidate {
                            path: path.clone(),
                            session_keys: BTreeSet::from([left.clone(), right.clone()]),
                            session_paths: BTreeMap::from([
                                (left, path.clone()),
                                (right, path.clone()),
                            ]),
                            warning_class: ProximityWarningClassV1::SameFile,
                            relation_kinds: Vec::new(),
                        }
                    })
                })
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
                        let (relation, relation_partial) = if let Some(cached) =
                            relation_cache.get(&path_pair)
                        {
                            cached.clone()
                        } else {
                            let (Some(left_nodes), Some(right_nodes)) =
                                (graph_nodes.get(&path_pair.0), graph_nodes.get(&path_pair.1))
                            else {
                                omissions.insert(FeedbackProximityOmissionV1::MissingCodeAddress);
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
                        if relation_partial {
                            omissions.insert(FeedbackProximityOmissionV1::CloneCoveragePartial);
                        }
                        let Some(relation) = relation else {
                            continue;
                        };
                        candidates.push(ProximityCandidate {
                            path: path_pair.0,
                            session_keys: BTreeSet::from([left_key.clone(), right_key.clone()]),
                            session_paths: BTreeMap::from([
                                (left_key.clone(), left_path.clone()),
                                (right_key.clone(), right_path.clone()),
                            ]),
                            warning_class: relation.warning_class,
                            relation_kinds: relation.kinds,
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
        if candidates.len() > MAX_PROXIMITY_EVIDENCE_V1 {
            omissions.insert(FeedbackProximityOmissionV1::EncounterLimit);
        }
        candidates.truncate(MAX_PROXIMITY_EVIDENCE_V1);
        let assembly = ProximityAssembly {
            normalized_worktree: &self.normalized_worktree,
            request,
            code_index_identity: code_index_identity.as_ref(),
            verified_graph_paths: &verified_graph_paths,
            observations: &observations,
            graph_nodes: &graph_nodes,
            session_edit_spans: &session_edit_spans,
            active: &active,
            expires_at,
        };
        let mut evidence = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            if let Some(item) = assembly.evidence_for(candidate, &mut omissions) {
                evidence.push(item);
            }
        }
        let coverage = if omissions.is_empty() {
            ProximityCoverageV1::Complete
        } else {
            ProximityCoverageV1::Partial
        };
        for item in &mut evidence {
            item.coverage = coverage;
            item.encounter.coverage = coverage;
        }
        CanonicalProximityEvidenceBatchV1 {
            evidence,
            coverage,
            source_generation,
            observed_at: request.observed_at,
            expires_at,
            omissions: omissions.into_iter().collect(),
        }
        .validated()
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
    sessions: RegisteredGlobalDbLeaseV1,
    code_graph: Arc<dyn CodeGraphProjectionReadPort>,
    scope: FeedbackScopeV1,
    worktree_root: PathBuf,
    code_index_identity: Arc<
        dyn crate::diagnostics_publication::CodeIndexPublicationIdentityPortV1,
    >,
) -> Option<SharedCanonicalProximityEvidenceAuthorityV1> {
    Some(Arc::new(
        ProductionProximityEvidenceAuthorityV1::new(sessions, code_graph, scope, worktree_root)?
            .with_code_index_identity(code_index_identity),
    ))
}

pub fn production_feedback_proximity_read_runtime_v1(
    sessions: RegisteredGlobalDbLeaseV1,
    code_graph: Arc<dyn CodeGraphProjectionReadPort>,
    scope: FeedbackScopeV1,
    worktree_root: PathBuf,
    code_index_identity: Arc<
        dyn crate::diagnostics_publication::CodeIndexPublicationIdentityPortV1,
    >,
) -> Option<FeedbackProximityReadRuntimeV1> {
    let evidence = production_proximity_evidence_authority_v1(
        sessions,
        code_graph,
        scope.clone(),
        worktree_root,
        code_index_identity,
    )?;
    FeedbackProximityReadRuntimeV1::new(scope, evidence)
}

fn activity_interval(hit: &SessionGitCorrelationHit) -> Option<FeedbackProximityIntervalV1> {
    let start = hit.first_ts.or(hit.committed_at)?;
    let end = hit.last_ts.or(hit.committed_at).unwrap_or(start);
    Some(FeedbackProximityIntervalV1 {
        start: UtcMicros(start.checked_mul(1_000_000)?),
        end: UtcMicros(end.checked_mul(1_000_000)?),
    })
}

fn branch_ref(branch: &str) -> Option<RefId> {
    let reference = if branch.starts_with("refs/") {
        branch.to_owned()
    } else {
        format!("refs/heads/{branch}")
    };
    RefId::new(reference).ok()
}

fn participant_graph_address(
    nodes: &[CodeGraphSymbolSummaryV1],
    ranges: &[SourceSpan],
) -> (Option<SourceSpan>, Option<SymbolOccurrenceId>) {
    let mut resolved = None::<(&CodeGraphSymbolSummaryV1, SourceSpan)>;
    for range in ranges {
        let Some(candidate) = resolve_edit_range_symbol(range, nodes) else {
            return (None, None);
        };
        match &resolved {
            Some((current, _)) if current.occurrence != candidate.0.occurrence => {
                return (None, None);
            }
            None => resolved = Some(candidate),
            _ => {}
        }
    }
    resolved.map_or((None, None), |(node, span)| {
        (Some(span), Some(node.occurrence.clone()))
    })
}

const MAX_GRAPH_RELATION_SEEDS: usize = 32;

fn relation_seeds(ids: &BTreeSet<SymbolOccurrenceId>) -> (Vec<SymbolOccurrenceId>, bool) {
    (
        ids.iter().take(MAX_GRAPH_RELATION_SEEDS).cloned().collect(),
        ids.len() > MAX_GRAPH_RELATION_SEEDS,
    )
}

#[derive(Clone)]
struct GraphRelation {
    warning_class: ProximityWarningClassV1,
    kinds: Vec<ProximityRelationPathKindV1>,
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
            }),
            false,
        );
    }

    let (left_seeds, left_truncated) = relation_seeds(&left_ids);
    let (right_seeds, right_truncated) = relation_seeds(&right_ids);
    let incomplete_seed_coverage = left_truncated || right_truncated;
    let Ok(left_edges) = graph.callees(
        &left_seeds,
        &[RelationEdgeKindV1::Calls],
        100_000,
        Arc::clone(&cancellation),
    ) else {
        return (None, true);
    };
    if left_edges.len() != left_seeds.len() {
        return (None, true);
    }
    for edges in &left_edges {
        if edges
            .iter()
            .any(|edge| right_ids.contains(&edge.edge.to_occurrence))
        {
            return (
                Some(GraphRelation {
                    warning_class: ProximityWarningClassV1::Neighborhood,
                    kinds: vec![ProximityRelationPathKindV1::DirectDependency],
                }),
                incomplete_seed_coverage,
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
    if right_edges.len() != right_seeds.len() {
        return (None, true);
    }
    for edges in &right_edges {
        if edges
            .iter()
            .any(|edge| left_ids.contains(&edge.edge.to_occurrence))
        {
            return (
                Some(GraphRelation {
                    warning_class: ProximityWarningClassV1::Neighborhood,
                    kinds: vec![ProximityRelationPathKindV1::DirectCaller],
                }),
                incomplete_seed_coverage,
            );
        }
    }

    // The direct relationship checks already read the first outgoing hop.
    // Reuse those generation-pinned batches for transitive discovery.
    let left_first_hop = left_edges
        .into_iter()
        .map(|edges| {
            edges
                .into_iter()
                .map(|edge| edge.edge.to_occurrence)
                .collect()
        })
        .collect();
    let right_first_hop = right_edges
        .into_iter()
        .map(|edges| {
            edges
                .into_iter()
                .map(|edge| edge.edge.to_occurrence)
                .collect()
        })
        .collect();
    // Evaluate bounded caller evidence before any outgoing second-hop work.
    // A proven shared caller must survive an unrelated dependency budget refusal.
    let (relation, partial) = graph_relation_neighborhood(
        graph,
        cancellation,
        &left_seeds,
        &right_seeds,
        left_first_hop,
        right_first_hop,
    );
    (relation, incomplete_seed_coverage || partial)
}

/// Collect only the bounded incoming caller closure needed for qualitative
/// proximity hints. A budget refusal cannot silently turn an uncertain
/// relationship into a negative result. The real producer supplies one
/// generation-pinned caller batch per hop; the injected reader lets tests
/// exercise cycles and incomplete batches rather than inspecting source text.
fn caller_neighborhood(
    seeds: &[SymbolOccurrenceId],
    max_symbols: usize,
    mut read_callers: impl FnMut(&[SymbolOccurrenceId]) -> Option<Vec<Vec<CodeGraphSymbolSummaryV1>>>,
) -> Option<(BTreeSet<SymbolOccurrenceId>, BTreeSet<SymbolOccurrenceId>)> {
    if max_symbols == 0 {
        return None;
    }
    let mut seen = seeds.iter().cloned().collect::<BTreeSet<_>>();
    // Duplicate seed identities must neither consume extra caller reads nor
    // grant extra traversal budget for unrelated neighbors.
    let seed_count = seen.len();
    let mut frontier = seen.iter().cloned().collect::<Vec<_>>();
    let mut callers = BTreeSet::new();
    let mut tests = BTreeSet::new();
    for depth in 1..=3 {
        if frontier.is_empty() {
            break;
        }
        let batches = read_callers(&frontier)?;
        if batches.len() != frontier.len() {
            return None;
        }
        let mut next = Vec::new();
        for neighbor in batches.into_iter().flatten() {
            let occurrence = neighbor.occurrence.clone();
            if !seen.insert(occurrence.clone()) {
                continue;
            }
            if seen.len() - seed_count > max_symbols {
                return None;
            }
            if depth <= 2 {
                callers.insert(occurrence.clone());
            }
            if neighbor
                .metadata
                .as_ref()
                .is_some_and(|metadata| metadata.kind.eq_ignore_ascii_case("test"))
            {
                tests.insert(occurrence.clone());
            }
            next.push(occurrence);
        }
        frontier = next;
    }
    Some((callers, tests))
}

/// Read the two-hop outgoing dependency closure without charging duplicate
/// seed identities to the graph's batch-wide relation budget. Fail closed if
/// a batch loses a seed's adjacency rather than turning missing evidence into
/// a negative relationship result.
fn dependency_neighborhood(
    seeds: &[SymbolOccurrenceId],
    first_hop: Vec<Vec<SymbolOccurrenceId>>,
    mut read_callees: impl FnMut(&[SymbolOccurrenceId]) -> Option<Vec<Vec<SymbolOccurrenceId>>>,
) -> Option<BTreeSet<SymbolOccurrenceId>> {
    let seed_ids = seeds.iter().cloned().collect::<BTreeSet<_>>();
    let mut frontier = seed_ids.iter().cloned().collect::<Vec<_>>();
    let mut dependencies = BTreeSet::new();
    let mut first_hop = Some(first_hop);
    for _ in 0..2 {
        if frontier.is_empty() {
            break;
        }
        let batches = if let Some(first) = first_hop.take() {
            first
        } else {
            read_callees(&frontier)?
        };
        if batches.len() != frontier.len() {
            return None;
        }
        frontier = batches
            .into_iter()
            .flatten()
            // Seed adjacency was already read in the first batch. A cycle back
            // to any root still contributes evidence, but needs no second read.
            .filter(|occurrence| dependencies.insert(occurrence.clone()))
            .filter(|occurrence| !seed_ids.contains(occurrence))
            .collect();
    }
    Some(dependencies)
}

fn graph_relation_neighborhood(
    graph: &CodeGraphInteractiveReader,
    cancellation: Arc<dyn tracedecay_graph_db::GraphCancellation>,
    left_seeds: &[SymbolOccurrenceId],
    right_seeds: &[SymbolOccurrenceId],
    left_first_hop: Vec<Vec<SymbolOccurrenceId>>,
    right_first_hop: Vec<Vec<SymbolOccurrenceId>>,
) -> (Option<GraphRelation>, bool) {
    graph_relation_neighborhood_with_reads(
        left_seeds,
        right_seeds,
        left_first_hop,
        right_first_hop,
        |frontier| {
            graph
                .callers(
                    frontier,
                    &[RelationEdgeKindV1::Calls],
                    100_000,
                    Arc::clone(&cancellation),
                )
                .ok()
                .map(|batches| {
                    batches
                        .into_iter()
                        .map(|edges| edges.into_iter().map(|edge| edge.neighbor).collect())
                        .collect()
                })
        },
        |frontier| {
            graph
                .callees(
                    frontier,
                    &[RelationEdgeKindV1::Calls],
                    100_000,
                    Arc::clone(&cancellation),
                )
                .ok()
                .map(|batches| {
                    batches
                        .into_iter()
                        .map(|edges| {
                            edges
                                .into_iter()
                                .map(|edge| edge.edge.to_occurrence)
                                .collect()
                        })
                        .collect()
                })
        },
    )
}

/// Use the incoming adjacency to establish a higher-precedence shared caller
/// before reading lower-precedence outgoing dependencies. The supplied first
/// hops have already passed the direct-adjacency completeness checks.
fn graph_relation_neighborhood_with_reads(
    left_seeds: &[SymbolOccurrenceId],
    right_seeds: &[SymbolOccurrenceId],
    left_first_hop: Vec<Vec<SymbolOccurrenceId>>,
    right_first_hop: Vec<Vec<SymbolOccurrenceId>>,
    mut read_callers: impl FnMut(&[SymbolOccurrenceId]) -> Option<Vec<Vec<CodeGraphSymbolSummaryV1>>>,
    mut read_callees: impl FnMut(&[SymbolOccurrenceId]) -> Option<Vec<Vec<SymbolOccurrenceId>>>,
) -> (Option<GraphRelation>, bool) {
    let Some((left_callers, left_tests)) =
        caller_neighborhood(left_seeds, 100_000, &mut read_callers)
    else {
        return (None, true);
    };
    let Some((right_callers, right_tests)) =
        caller_neighborhood(right_seeds, 100_000, &mut read_callers)
    else {
        return (None, true);
    };
    if !left_callers.is_disjoint(&right_callers) {
        return (
            Some(GraphRelation {
                warning_class: ProximityWarningClassV1::SharedCaller,
                kinds: vec![ProximityRelationPathKindV1::TransitiveCaller],
            }),
            false,
        );
    }

    // The incoming closure can already prove an affected test even when a
    // later, higher-precedence dependency read cannot finish. Retain that
    // positive evidence as partial rather than incorrectly returning no hint.
    let shared_test = (!left_tests.is_disjoint(&right_tests)).then(|| GraphRelation {
        warning_class: ProximityWarningClassV1::SharedTest,
        kinds: vec![ProximityRelationPathKindV1::AffectedTest],
    });
    let Some(left_dependencies) =
        dependency_neighborhood(left_seeds, left_first_hop, &mut read_callees)
    else {
        return (shared_test, true);
    };
    let Some(right_dependencies) =
        dependency_neighborhood(right_seeds, right_first_hop, &mut read_callees)
    else {
        return (shared_test, true);
    };
    if !left_dependencies.is_disjoint(&right_dependencies) {
        return (
            Some(GraphRelation {
                warning_class: ProximityWarningClassV1::SharedDependency,
                kinds: vec![ProximityRelationPathKindV1::TransitiveDependency],
            }),
            false,
        );
    }
    (shared_test, false)
}

fn exact_graph_address(
    nodes: &[CodeGraphSymbolSummaryV1],
    session_ranges: &[&[SourceSpan]],
) -> Option<(SourceSpan, SymbolOccurrenceId)> {
    if session_ranges.len() < 2 {
        return None;
    }
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

    fn test_callable(id: &str, span: SourceSpan) -> CodeGraphSymbolSummaryV1 {
        let mut node = callable(id, span);
        let digest = format!("sha256:{}", "a".repeat(64));
        node.metadata = Some(tracedecay_code_index::lineage::LineageSymbolRecordV1 {
            occurrence: node.occurrence.clone(),
            identity: digest.clone().try_into().unwrap(),
            qualified_name: "tests::shared".to_owned(),
            simple_name: "shared".to_owned(),
            kind: "TeSt".to_owned(),
            visibility: "private".to_owned(),
            branches: 0,
            loops: 0,
            max_nesting: 0,
            complexity_analysis: tracedecay_domain::ComplexityAnalysisV1::Complete,
            line_span: 1,
            start_line: 0,
            signature: None,
            docstring: None,
            is_async: false,
            derives: Vec::new(),
            skip_test_coverage: false,
            file_identity: digest.try_into().unwrap(),
            content_digest: ContentDigest::of_bytes(b"shared test"),
        });
        node
    }

    #[test]
    fn same_symbol_requires_each_typed_edit_range_to_resolve_to_that_symbol() {
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
            &nodes,
            &[std::slice::from_ref(&alpha), std::slice::from_ref(&alpha)],
        )
        .unwrap();
        assert_eq!(same.1.as_str(), "symbol.alpha");
        assert!(
            exact_graph_address(
                &nodes,
                &[std::slice::from_ref(&alpha), std::slice::from_ref(&beta)],
            )
            .is_none()
        );
        assert!(exact_graph_address(&nodes, &[&[], &[]]).is_none());
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
    fn caller_neighborhood_preserves_depth_bounds_cycles_and_refusals() {
        let root = SymbolOccurrenceId::new("symbol.root").unwrap();
        let first = SymbolOccurrenceId::new("symbol.first").unwrap();
        let second = SymbolOccurrenceId::new("symbol.second").unwrap();
        let third = SymbolOccurrenceId::new("symbol.third").unwrap();
        let node = |id| {
            callable(
                id,
                SourceSpan {
                    start_byte: 0,
                    end_byte: 1,
                },
            )
        };
        let mut reads = 0;
        let (callers, tests) = caller_neighborhood(std::slice::from_ref(&root), 3, |frontier| {
            reads += 1;
            Some(
                frontier
                    .iter()
                    .map(|id| match id.as_str() {
                        "symbol.root" => vec![node("symbol.first")],
                        "symbol.first" => vec![node("symbol.root"), node("symbol.second")],
                        "symbol.second" => vec![node("symbol.third")],
                        _ => Vec::new(),
                    })
                    .collect(),
            )
        })
        .expect("complete bounded caller closure");
        assert_eq!(reads, 3, "one generation-pinned caller batch per depth");
        assert_eq!(callers, BTreeSet::from([first, second]));
        assert!(!callers.contains(&third), "third hop is not a direct hint");
        assert!(tests.is_empty());

        assert!(
            caller_neighborhood(std::slice::from_ref(&root), 2, |frontier| {
                Some(
                    frontier
                        .iter()
                        .map(|id| match id.as_str() {
                            "symbol.root" => vec![node("symbol.first")],
                            "symbol.first" => vec![node("symbol.second")],
                            "symbol.second" => vec![node("symbol.third")],
                            _ => Vec::new(),
                        })
                        .collect(),
                )
            })
            .is_none(),
            "exhaustion is partial, never a false negative"
        );
        assert!(
            caller_neighborhood(std::slice::from_ref(&root), 10, |_| Some(Vec::new())).is_none(),
            "incomplete per-seed evidence is refused"
        );

        let repeated_roots = [root.clone(), root.clone()];
        let mut duplicate_reads = 0;
        assert!(
            caller_neighborhood(&repeated_roots, 2, |frontier| {
                assert_eq!(frontier.len(), 1, "each unique seed is read once");
                duplicate_reads += 1;
                Some(
                    frontier
                        .iter()
                        .map(|id| match id.as_str() {
                            "symbol.root" => vec![node("symbol.first")],
                            "symbol.first" => vec![node("symbol.second")],
                            "symbol.second" => vec![node("symbol.third")],
                            _ => Vec::new(),
                        })
                        .collect(),
                )
            })
            .is_none(),
            "duplicate roots must not extend the unique-neighbor budget"
        );
        assert_eq!(duplicate_reads, 3);
    }

    #[test]
    fn relation_seeds_mark_unexamined_symbols_as_partial() {
        let symbols = (0..33)
            .map(|index| SymbolOccurrenceId::new(format!("symbol.{index:02}")).unwrap())
            .collect::<BTreeSet<_>>();
        let (seeds, truncated) = relation_seeds(&symbols);
        assert_eq!(seeds.len(), MAX_GRAPH_RELATION_SEEDS);
        assert!(
            truncated,
            "skipped graph nodes make a negative hint inconclusive"
        );
        assert!(!seeds.contains(&SymbolOccurrenceId::new("symbol.32").unwrap()));
        let (complete_seeds, incomplete) = relation_seeds(&seeds.iter().cloned().collect());
        assert!(!incomplete);
        assert_eq!(complete_seeds, seeds);
    }

    #[test]
    fn dependency_neighborhood_reuses_first_hop_and_refuses_missing_adjacency() {
        let root = SymbolOccurrenceId::new("symbol.root").unwrap();
        let direct = SymbolOccurrenceId::new("symbol.direct").unwrap();
        let transitive = SymbolOccurrenceId::new("symbol.transitive").unwrap();
        let mut reads = 0;
        let result = dependency_neighborhood(
            &[root.clone(), root.clone()],
            vec![vec![direct.clone()]],
            |frontier| {
                reads += 1;
                assert_eq!(
                    frontier,
                    std::slice::from_ref(&direct),
                    "only the second hop is read"
                );
                Some(vec![vec![root.clone(), transitive.clone()]])
            },
        )
        .expect("two complete dependency hops");
        assert_eq!(reads, 1, "the first direct-edge read is reused");
        assert_eq!(
            result,
            BTreeSet::from([root.clone(), direct.clone(), transitive])
        );

        assert!(
            dependency_neighborhood(std::slice::from_ref(&root), Vec::new(), |_| {
                panic!("incomplete first-hop evidence must not trigger another read")
            })
            .is_none(),
            "missing direct adjacency cannot prove absence of relationships"
        );
        assert!(
            dependency_neighborhood(std::slice::from_ref(&root), vec![vec![direct]], |_| Some(
                Vec::new()
            ))
            .is_none(),
            "missing second-hop adjacency cannot prove absence of relationships"
        );
    }

    #[test]
    fn caller_neighborhood_retains_third_hop_tests_without_expanding_them() {
        let root = SymbolOccurrenceId::new("symbol.root").unwrap();
        let first = SymbolOccurrenceId::new("symbol.first").unwrap();
        let second = SymbolOccurrenceId::new("symbol.second").unwrap();
        let test_id = SymbolOccurrenceId::new("symbol.test").unwrap();
        let span = SourceSpan {
            start_byte: 0,
            end_byte: 1,
        };
        let test_node = test_callable(test_id.as_str(), span);
        let mut frontiers = Vec::new();
        let (callers, tests) = caller_neighborhood(std::slice::from_ref(&root), 3, |frontier| {
            frontiers.push(frontier.to_vec());
            assert_eq!(frontier.len(), 1);
            let neighbors = match frontier[0].as_str() {
                "symbol.root" => vec![callable(first.as_str(), span)],
                "symbol.first" => vec![callable(second.as_str(), span)],
                "symbol.second" => vec![test_node.clone(), test_node.clone()],
                _ => panic!("third-hop tests must not trigger a fourth graph read"),
            };
            Some(vec![neighbors])
        })
        .expect("three unique neighbors fit the budget exactly");
        assert_eq!(
            frontiers,
            vec![vec![root], vec![first.clone()], vec![second.clone()]]
        );
        assert_eq!(callers, BTreeSet::from([first, second]));
        assert_eq!(tests, BTreeSet::from([test_id.clone()]));
        assert!(!callers.contains(&test_id));
    }

    #[test]
    fn caller_neighborhood_refuses_failed_reads_at_every_hop() {
        let root = SymbolOccurrenceId::new("symbol.root").unwrap();
        for failed_hop in 1..=3 {
            let mut reads = 0;
            let result = caller_neighborhood(std::slice::from_ref(&root), 3, |_| {
                reads += 1;
                if reads == failed_hop {
                    return None;
                }
                Some(vec![vec![callable(
                    &format!("symbol.hop.{reads}"),
                    SourceSpan {
                        start_byte: 0,
                        end_byte: 1,
                    },
                )]])
            });
            assert!(
                result.is_none(),
                "hop {failed_hop} cannot publish partial evidence as complete"
            );
            assert_eq!(reads, failed_hop, "a failed read must stop traversal");
        }
    }

    #[test]
    fn empty_neighborhoods_do_not_issue_additional_graph_reads() {
        let empty = caller_neighborhood(&[], 3, |_| {
            panic!("an empty caller seed set requires no read")
        })
        .expect("empty caller closure is complete");
        assert!(empty.0.is_empty());
        assert!(empty.1.is_empty());

        let root = SymbolOccurrenceId::new("symbol.root").unwrap();
        let dependencies =
            dependency_neighborhood(std::slice::from_ref(&root), vec![Vec::new()], |_| {
                panic!("empty first-hop adjacency requires no second-hop read")
            })
            .expect("empty dependency closure is complete");
        assert!(dependencies.is_empty());
    }

    #[test]
    fn dependency_neighborhood_reuses_adjacency_between_distinct_seeds() {
        let first = SymbolOccurrenceId::new("symbol.first").unwrap();
        let second = SymbolOccurrenceId::new("symbol.second").unwrap();
        let direct = SymbolOccurrenceId::new("symbol.direct").unwrap();
        let transitive = SymbolOccurrenceId::new("symbol.transitive").unwrap();
        let mut reads = 0;
        let result = dependency_neighborhood(
            &[first.clone(), second.clone()],
            vec![vec![second.clone()], vec![direct.clone()]],
            |frontier| {
                reads += 1;
                assert_eq!(
                    frontier,
                    std::slice::from_ref(&direct),
                    "seed adjacency is already supplied by the first hop"
                );
                Some(vec![vec![first.clone(), transitive.clone()]])
            },
        )
        .expect("overlapping roots retain two-hop evidence");
        assert_eq!(reads, 1);
        assert_eq!(result, BTreeSet::from([first, second, direct, transitive]));
    }

    #[test]
    fn dependency_neighborhood_preserves_seed_cycles_without_extra_reads() {
        let first = SymbolOccurrenceId::new("symbol.first").unwrap();
        let second = SymbolOccurrenceId::new("symbol.second").unwrap();
        let result = dependency_neighborhood(
            &[first.clone(), second.clone()],
            vec![vec![first.clone(), second.clone()], vec![first.clone()]],
            |_| panic!("seed cycles must not issue another adjacency read"),
        )
        .expect("supplied adjacency completely covers the seed cycle");
        assert_eq!(result, BTreeSet::from([first, second]));
    }

    #[test]
    fn shared_third_hop_test_survives_unavailable_dependency_evidence() {
        let symbol = |id| SymbolOccurrenceId::new(id).unwrap();
        let left = symbol("symbol.left");
        let right = symbol("symbol.right");
        let span = SourceSpan {
            start_byte: 0,
            end_byte: 1,
        };
        let shared_test = test_callable("symbol.shared-test", span);
        let shared_dependency = symbol("symbol.shared-dependency");

        // One side may reject its second hop after both three-hop caller
        // closures have proven a shared affected test. A complete dependency
        // proof still outranks the test, but refusal cannot erase it.
        for failed_second_hop in [
            Some("symbol.left-direct"),
            Some("symbol.right-direct"),
            None,
        ] {
            let mut reads = 0;
            let (relation, partial) = graph_relation_neighborhood_with_reads(
                std::slice::from_ref(&left),
                std::slice::from_ref(&right),
                vec![vec![symbol("symbol.left-direct")]],
                vec![vec![symbol("symbol.right-direct")]],
                |frontier| {
                    Some(
                        frontier
                            .iter()
                            .map(|seed| match seed.as_str() {
                                "symbol.left" => vec![callable("symbol.left-1", span)],
                                "symbol.left-1" => vec![callable("symbol.left-2", span)],
                                "symbol.left-2" => vec![shared_test.clone()],
                                "symbol.right" => vec![callable("symbol.right-1", span)],
                                "symbol.right-1" => vec![callable("symbol.right-2", span)],
                                "symbol.right-2" => vec![shared_test.clone()],
                                _ => panic!("third-hop tests do not require further reads"),
                            })
                            .collect(),
                    )
                },
                |frontier| {
                    reads += 1;
                    if frontier
                        .iter()
                        .any(|seed| Some(seed.as_str()) == failed_second_hop)
                    {
                        return None;
                    }
                    Some(
                        frontier
                            .iter()
                            .map(|_| vec![shared_dependency.clone()])
                            .collect(),
                    )
                },
            );
            let relation = relation.expect("proven evidence must not be discarded");
            let failed = failed_second_hop.is_some();
            assert_eq!(
                relation.warning_class,
                if failed {
                    ProximityWarningClassV1::SharedTest
                } else {
                    ProximityWarningClassV1::SharedDependency
                }
            );
            assert_eq!(
                relation.kinds,
                vec![if failed {
                    ProximityRelationPathKindV1::AffectedTest
                } else {
                    ProximityRelationPathKindV1::TransitiveDependency
                }]
            );
            assert_eq!(partial, failed, "unread adjacency makes coverage partial");
            assert_eq!(
                reads,
                if failed_second_hop == Some("symbol.left-direct") {
                    1
                } else {
                    2
                },
                "stop at the refusing hop, retaining the known test relation"
            );
        }
    }

    #[test]
    fn shared_caller_does_not_read_discarded_dependency_hops() {
        let left = SymbolOccurrenceId::new("symbol.left").unwrap();
        let right = SymbolOccurrenceId::new("symbol.right").unwrap();
        let span = SourceSpan {
            start_byte: 0,
            end_byte: 1,
        };
        let common = callable("symbol.shared-caller", span);
        let mut caller_reads = 0;
        let (relation, partial) = graph_relation_neighborhood_with_reads(
            std::slice::from_ref(&left),
            std::slice::from_ref(&right),
            vec![vec![
                SymbolOccurrenceId::new("symbol.left-dependency").unwrap(),
            ]],
            vec![vec![
                SymbolOccurrenceId::new("symbol.right-dependency").unwrap(),
            ]],
            |frontier| {
                caller_reads += 1;
                Some(
                    frontier
                        .iter()
                        .map(|seed| match seed.as_str() {
                            "symbol.left" | "symbol.right" => vec![common.clone()],
                            "symbol.shared-caller" => Vec::new(),
                            _ => panic!("unexpected caller frontier"),
                        })
                        .collect(),
                )
            },
            |_| panic!("shared caller must not be erased by an unused dependency read"),
        );
        let relation = relation.expect("proven shared caller must survive");
        assert_eq!(
            relation.warning_class,
            ProximityWarningClassV1::SharedCaller
        );
        assert_eq!(
            relation.kinds,
            vec![ProximityRelationPathKindV1::TransitiveCaller]
        );
        assert!(!partial);
        assert_eq!(caller_reads, 4, "both incoming closures are fully read");
    }

    #[test]
    fn unmatched_callers_still_read_two_hop_shared_dependencies() {
        let left = SymbolOccurrenceId::new("symbol.left").unwrap();
        let right = SymbolOccurrenceId::new("symbol.right").unwrap();
        let common = SymbolOccurrenceId::new("symbol.common").unwrap();
        let mut outgoing_reads = 0;
        let (relation, partial) = graph_relation_neighborhood_with_reads(
            std::slice::from_ref(&left),
            std::slice::from_ref(&right),
            vec![vec![
                SymbolOccurrenceId::new("symbol.left-dependency").unwrap(),
            ]],
            vec![vec![
                SymbolOccurrenceId::new("symbol.right-dependency").unwrap(),
            ]],
            |frontier| Some(vec![Vec::new(); frontier.len()]),
            |frontier| {
                outgoing_reads += 1;
                Some(
                    frontier
                        .iter()
                        .map(|seed| match seed.as_str() {
                            "symbol.left-dependency" | "symbol.right-dependency" => {
                                vec![common.clone()]
                            }
                            _ => panic!("first-hop adjacency must be reused"),
                        })
                        .collect(),
                )
            },
        );
        let relation = relation.expect("real two-hop dependency is preserved");
        assert_eq!(
            relation.warning_class,
            ProximityWarningClassV1::SharedDependency
        );
        assert_eq!(
            relation.kinds,
            vec![ProximityRelationPathKindV1::TransitiveDependency]
        );
        assert!(!partial);
        assert_eq!(outgoing_reads, 2, "one extra read per side");
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

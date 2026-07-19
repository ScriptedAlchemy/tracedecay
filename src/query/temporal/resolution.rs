use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Deref,
};

use serde::Serialize;
use tracedecay_domain::{
    LogicalCopyRecordV1, MessageOccurrenceIdV1, RetrievalAnchorId, SessionAuthorityClassV1,
    SessionId, SessionSummaryIdV1, SessionSummaryRecordV1, TemporalAssertionKindV1,
    TemporalAssertionRecordV1, TemporalModeV1, TemporalValidityV1, UtcMicros,
};

use super::ports::{ExecutionControl, TemporalPortError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolutionEvidence {
    pub authority: SessionAuthorityClassV1,
    authorized: bool,
    pub supporting_anchor_ids: BTreeSet<RetrievalAnchorId>,
}

impl ResolutionEvidence {
    pub fn new(authority: SessionAuthorityClassV1, authorization: ValidatedAuthorization) -> Self {
        Self {
            authority,
            authorized: authorization.is_authorized(),
            supporting_anchor_ids: BTreeSet::new(),
        }
    }

    pub const fn is_authorized(&self) -> bool {
        self.authorized
    }

    pub fn with_supporting_anchor(mut self, anchor_id: RetrievalAnchorId) -> Self {
        self.supporting_anchor_ids.insert(anchor_id);
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidatedAuthorization {
    Authorized,
    Unauthorized,
}

impl ValidatedAuthorization {
    pub const fn is_authorized(self) -> bool {
        matches!(self, Self::Authorized)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolutionInputError {
    UnauthorizedAssertion,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolutionOccurrence {
    pub occurrence_id: MessageOccurrenceIdV1,
    pub anchor_id: RetrievalAnchorId,
    pub knowledge_at: UtcMicros,
    pub valid_time: TemporalValidityV1,
    pub evidence: ResolutionEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolutionAssertion {
    pub kind: TemporalAssertionKindV1,
    pub subject_anchor_id: RetrievalAnchorId,
    pub object_anchor_id: RetrievalAnchorId,
    pub knowledge_at: UtcMicros,
    pub valid_time: TemporalValidityV1,
    pub evidence: ResolutionEvidence,
}

impl ResolutionAssertion {
    pub fn from_record(
        assertion: &TemporalAssertionRecordV1,
        authorization: ValidatedAuthorization,
    ) -> Result<Self, ResolutionInputError> {
        if !authorization.is_authorized() {
            return Err(ResolutionInputError::UnauthorizedAssertion);
        }
        Ok(Self {
            kind: assertion.kind,
            subject_anchor_id: assertion.subject_anchor_id.clone(),
            object_anchor_id: assertion.object_anchor_id.clone(),
            knowledge_at: assertion.knowledge_at,
            valid_time: assertion.valid_time,
            evidence: ResolutionEvidence::new(assertion.evidence.authority, authorization)
                .with_supporting_anchor(assertion.evidence.source_anchor_id.clone()),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedOccurrence {
    pub occurrence: ResolutionOccurrence,
    pub representative_id: MessageOccurrenceIdV1,
    pub conflicted: bool,
    pub uncertain: bool,
    pub supporting_anchor_ids: BTreeSet<RetrievalAnchorId>,
}

impl ResolvedOccurrence {
    pub const fn certainty(&self) -> ResolutionCertainty {
        if self.uncertain {
            ResolutionCertainty::AuthorizedUnknown
        } else {
            ResolutionCertainty::Known
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolutionCertainty {
    Known,
    AuthorizedUnknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ResolutionLineageEdgeKind {
    Correction,
    Contradiction,
    Supersession,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolutionLineageEdge {
    pub kind: ResolutionLineageEdgeKind,
    pub subject_anchor_id: RetrievalAnchorId,
    pub object_anchor_id: RetrievalAnchorId,
    pub knowledge_at: UtcMicros,
    pub evidence: ResolutionEvidence,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TemporalResolution {
    pub occurrences: Vec<ResolvedOccurrence>,
    pub lineage_edges: Vec<ResolutionLineageEdge>,
}

impl Deref for TemporalResolution {
    type Target = [ResolvedOccurrence];

    fn deref(&self) -> &Self::Target {
        &self.occurrences
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolutionCheckpoint {
    Occurrence,
    Copy,
    Assertion,
    Relation,
    Materialization,
    Evolution,
}

pub fn resolve_temporal(
    occurrences: &[ResolutionOccurrence],
    copies: &[LogicalCopyRecordV1],
    assertions: &[ResolutionAssertion],
    mode: TemporalModeV1,
) -> Result<TemporalResolution, TemporalPortError> {
    resolve_temporal_controlled(
        occurrences,
        copies,
        assertions,
        mode,
        &ExecutionControl::default(),
    )
}

pub fn resolve_temporal_controlled(
    occurrences: &[ResolutionOccurrence],
    copies: &[LogicalCopyRecordV1],
    assertions: &[ResolutionAssertion],
    mode: TemporalModeV1,
    control: &ExecutionControl,
) -> Result<TemporalResolution, TemporalPortError> {
    let mut hook = |_checkpoint| Ok(());
    resolve_temporal_with_checkpoints(occurrences, copies, assertions, mode, control, &mut hook)
}

pub fn resolve_temporal_with_checkpoints(
    occurrences: &[ResolutionOccurrence],
    copies: &[LogicalCopyRecordV1],
    assertions: &[ResolutionAssertion],
    mode: TemporalModeV1,
    control: &ExecutionControl,
    hook: &mut dyn FnMut(ResolutionCheckpoint) -> Result<(), TemporalPortError>,
) -> Result<TemporalResolution, TemporalPortError> {
    let mut eligible = Vec::with_capacity(occurrences.len());
    for occurrence in occurrences {
        checkpoint(control, hook, ResolutionCheckpoint::Occurrence)?;
        if occurrence.evidence.is_authorized()
            && occurrence
                .valid_time
                .is_representative_at(occurrence.knowledge_at, mode)
        {
            eligible.push(occurrence.clone());
        }
    }
    let eligible_ids = eligible
        .iter()
        .map(|occurrence| occurrence.occurrence_id.clone())
        .collect::<BTreeSet<_>>();
    let eligible_anchors = eligible
        .iter()
        .map(|occurrence| occurrence.anchor_id.clone())
        .collect::<BTreeSet<_>>();
    let copy_sources = copy_sources(copies, control, hook)?;
    let mut eligible_assertions = Vec::with_capacity(assertions.len());
    for assertion in assertions {
        checkpoint(control, hook, ResolutionCheckpoint::Assertion)?;
        if assertion.evidence.is_authorized()
            && assertion
                .valid_time
                .is_representative_at(assertion.knowledge_at, mode)
            && eligible_anchors.contains(&assertion.subject_anchor_id)
            && eligible_anchors.contains(&assertion.object_anchor_id)
        {
            eligible_assertions.push(assertion);
        }
    }

    let by_anchor = eligible
        .iter()
        .map(|occurrence| (occurrence.anchor_id.clone(), occurrence))
        .collect::<BTreeMap<_, _>>();
    let support = collect_support(&eligible, &eligible_assertions, control, hook)?;
    let mut suppressed_anchors = BTreeSet::new();
    let mut conflict_anchors = BTreeSet::new();
    if matches!(mode, TemporalModeV1::Current | TemporalModeV1::AsOf { .. }) {
        // (suppressor, suppressed) edges from successful Corrects/Supersedes only.
        let mut suppression_edges = BTreeSet::<(RetrievalAnchorId, RetrievalAnchorId)>::new();
        for assertion in &eligible_assertions {
            checkpoint(control, hook, ResolutionCheckpoint::Relation)?;
            let subject = by_anchor[&assertion.subject_anchor_id];
            let object = by_anchor[&assertion.object_anchor_id];
            let subject_strength = evidence_strength(subject, &support);
            let object_strength = evidence_strength(object, &support);
            let assertion_rank = authority_rank(assertion.evidence.authority);
            match assertion.kind {
                TemporalAssertionKindV1::Corrects | TemporalAssertionKindV1::Supersedes => {
                    if assertion_rank >= authority_rank(object.evidence.authority)
                        && subject_strength >= object_strength
                    {
                        suppressed_anchors.insert(assertion.object_anchor_id.clone());
                        suppression_edges.insert((
                            assertion.subject_anchor_id.clone(),
                            assertion.object_anchor_id.clone(),
                        ));
                    } else {
                        conflict_anchors.insert(assertion.subject_anchor_id.clone());
                        conflict_anchors.insert(assertion.object_anchor_id.clone());
                    }
                }
                TemporalAssertionKindV1::Contradicts => {
                    if subject_strength > object_strength
                        && assertion_rank >= authority_rank(object.evidence.authority)
                    {
                        suppressed_anchors.insert(assertion.object_anchor_id.clone());
                    } else if object_strength > subject_strength
                        && assertion_rank >= authority_rank(subject.evidence.authority)
                    {
                        suppressed_anchors.insert(assertion.subject_anchor_id.clone());
                    } else {
                        conflict_anchors.insert(assertion.subject_anchor_id.clone());
                        conflict_anchors.insert(assertion.object_anchor_id.clone());
                    }
                }
                TemporalAssertionKindV1::Supports => {}
            }
        }
        // Reciprocal wipe only: A suppresses B and B suppresses A.
        // Ordinary chains (C→B→A) must keep the tip and leave history suppressed.
        for (subject, object) in &suppression_edges {
            checkpoint(control, hook, ResolutionCheckpoint::Relation)?;
            if suppression_edges.contains(&(object.clone(), subject.clone())) {
                conflict_anchors.insert(subject.clone());
                conflict_anchors.insert(object.clone());
            }
        }
        suppressed_anchors.retain(|anchor| !conflict_anchors.contains(anchor));
    } else {
        conflict_anchors.extend(
            eligible_assertions
                .iter()
                .filter(|assertion| assertion.kind == TemporalAssertionKindV1::Contradicts)
                .flat_map(|assertion| {
                    [
                        assertion.subject_anchor_id.clone(),
                        assertion.object_anchor_id.clone(),
                    ]
                }),
        );
    }

    let mut resolved = Vec::with_capacity(eligible.len());
    for occurrence in eligible {
        checkpoint(control, hook, ResolutionCheckpoint::Materialization)?;
        if suppressed_anchors.contains(&occurrence.anchor_id) {
            continue;
        }
        let representative_id = copy_root(
            &occurrence.occurrence_id,
            &copy_sources,
            &eligible_ids,
            control,
            hook,
        )?;
        let collapse_copy = !matches!(mode, TemporalModeV1::Forensic)
            && representative_id != occurrence.occurrence_id
            && eligible_ids.contains(&representative_id);
        if collapse_copy {
            continue;
        }
        let conflicted = conflict_anchors.contains(&occurrence.anchor_id);
        let supporting_anchor_ids = support
            .get(&occurrence.anchor_id)
            .cloned()
            .unwrap_or_default();
        resolved.push(ResolvedOccurrence {
            uncertain: occurrence.valid_time == TemporalValidityV1::Unknown,
            occurrence,
            representative_id,
            conflicted,
            supporting_anchor_ids,
        });
    }
    let mut lineage_edges = Vec::new();
    for assertion in &eligible_assertions {
        checkpoint(control, hook, ResolutionCheckpoint::Relation)?;
        let kind = match assertion.kind {
            TemporalAssertionKindV1::Corrects => ResolutionLineageEdgeKind::Correction,
            TemporalAssertionKindV1::Contradicts => ResolutionLineageEdgeKind::Contradiction,
            TemporalAssertionKindV1::Supersedes => ResolutionLineageEdgeKind::Supersession,
            TemporalAssertionKindV1::Supports => continue,
        };
        lineage_edges.push(ResolutionLineageEdge {
            kind,
            subject_anchor_id: assertion.subject_anchor_id.clone(),
            object_anchor_id: assertion.object_anchor_id.clone(),
            knowledge_at: assertion.knowledge_at,
            evidence: assertion.evidence.clone(),
        });
    }
    if mode == TemporalModeV1::Evolution {
        resolved = order_evolution(resolved, &eligible_assertions, control, hook)?;
        let positions = resolved
            .iter()
            .enumerate()
            .map(|(index, item)| (item.occurrence.anchor_id.clone(), index))
            .collect::<BTreeMap<_, _>>();
        lineage_edges.sort_by(|left, right| {
            positions
                .get(&left.object_anchor_id)
                .copied()
                .unwrap_or(usize::MAX)
                .cmp(
                    &positions
                        .get(&right.object_anchor_id)
                        .copied()
                        .unwrap_or(usize::MAX),
                )
                .then_with(|| {
                    positions
                        .get(&left.subject_anchor_id)
                        .copied()
                        .unwrap_or(usize::MAX)
                        .cmp(
                            &positions
                                .get(&right.subject_anchor_id)
                                .copied()
                                .unwrap_or(usize::MAX),
                        )
                })
                .then_with(|| left.kind.cmp(&right.kind))
                .then_with(|| left.knowledge_at.cmp(&right.knowledge_at))
                .then_with(|| left.object_anchor_id.cmp(&right.object_anchor_id))
                .then_with(|| left.subject_anchor_id.cmp(&right.subject_anchor_id))
        });
    } else {
        resolved.sort_by(stable_occurrence_order);
        lineage_edges.sort_by(|left, right| {
            left.knowledge_at
                .cmp(&right.knowledge_at)
                .then_with(|| left.object_anchor_id.cmp(&right.object_anchor_id))
                .then_with(|| left.subject_anchor_id.cmp(&right.subject_anchor_id))
                .then_with(|| left.kind.cmp(&right.kind))
        });
    }
    checkpoint(control, hook, ResolutionCheckpoint::Materialization)?;
    Ok(TemporalResolution {
        occurrences: resolved,
        lineage_edges,
    })
}

fn checkpoint(
    control: &ExecutionControl,
    hook: &mut dyn FnMut(ResolutionCheckpoint) -> Result<(), TemporalPortError>,
    phase: ResolutionCheckpoint,
) -> Result<(), TemporalPortError> {
    control.checkpoint()?;
    hook(phase)
}

fn collect_support(
    occurrences: &[ResolutionOccurrence],
    assertions: &[&ResolutionAssertion],
    control: &ExecutionControl,
    hook: &mut dyn FnMut(ResolutionCheckpoint) -> Result<(), TemporalPortError>,
) -> Result<BTreeMap<RetrievalAnchorId, BTreeSet<RetrievalAnchorId>>, TemporalPortError> {
    let eligible_anchors = occurrences
        .iter()
        .map(|occurrence| occurrence.anchor_id.clone())
        .collect::<BTreeSet<_>>();
    let mut support = BTreeMap::new();
    for occurrence in occurrences {
        checkpoint(control, hook, ResolutionCheckpoint::Relation)?;
        support.insert(
            occurrence.anchor_id.clone(),
            occurrence
                .evidence
                .supporting_anchor_ids
                .iter()
                .filter(|anchor| eligible_anchors.contains(*anchor))
                .cloned()
                .collect::<BTreeSet<_>>(),
        );
    }
    for assertion in assertions {
        checkpoint(control, hook, ResolutionCheckpoint::Relation)?;
        if assertion.kind == TemporalAssertionKindV1::Supports {
            let anchors = support
                .entry(assertion.object_anchor_id.clone())
                .or_default();
            anchors.insert(assertion.subject_anchor_id.clone());
            anchors.extend(
                assertion
                    .evidence
                    .supporting_anchor_ids
                    .iter()
                    .filter(|anchor| eligible_anchors.contains(*anchor))
                    .cloned(),
            );
        }
    }
    Ok(support)
}

fn evidence_strength(
    occurrence: &ResolutionOccurrence,
    support: &BTreeMap<RetrievalAnchorId, BTreeSet<RetrievalAnchorId>>,
) -> (u8, usize) {
    (
        authority_rank(occurrence.evidence.authority),
        support
            .get(&occurrence.anchor_id)
            .map(BTreeSet::len)
            .unwrap_or_default(),
    )
}

const fn authority_rank(authority: SessionAuthorityClassV1) -> u8 {
    match authority {
        SessionAuthorityClassV1::ProviderNative => 5,
        SessionAuthorityClassV1::CanonicalObservation => 4,
        SessionAuthorityClassV1::ExplicitAnchorAssertion => 3,
        SessionAuthorityClassV1::ImmutableSummary => 2,
        SessionAuthorityClassV1::DerivedProjection => 1,
    }
}

fn stable_occurrence_order(
    left: &ResolvedOccurrence,
    right: &ResolvedOccurrence,
) -> std::cmp::Ordering {
    left.occurrence
        .knowledge_at
        .cmp(&right.occurrence.knowledge_at)
        .then_with(|| {
            left.occurrence
                .occurrence_id
                .cmp(&right.occurrence.occurrence_id)
        })
}

fn order_evolution(
    resolved: Vec<ResolvedOccurrence>,
    assertions: &[&ResolutionAssertion],
    control: &ExecutionControl,
    hook: &mut dyn FnMut(ResolutionCheckpoint) -> Result<(), TemporalPortError>,
) -> Result<Vec<ResolvedOccurrence>, TemporalPortError> {
    let mut by_anchor = resolved
        .into_iter()
        .map(|item| (item.occurrence.anchor_id.clone(), item))
        .collect::<BTreeMap<_, _>>();
    let mut descendants = BTreeMap::<RetrievalAnchorId, BTreeSet<RetrievalAnchorId>>::new();
    let mut indegree = by_anchor
        .keys()
        .cloned()
        .map(|anchor| (anchor, 0_usize))
        .collect::<BTreeMap<_, _>>();
    for assertion in assertions.iter().filter(|assertion| {
        matches!(
            assertion.kind,
            TemporalAssertionKindV1::Corrects | TemporalAssertionKindV1::Supersedes
        )
    }) {
        checkpoint(control, hook, ResolutionCheckpoint::Evolution)?;
        if by_anchor.contains_key(&assertion.subject_anchor_id)
            && by_anchor.contains_key(&assertion.object_anchor_id)
            && descendants
                .entry(assertion.object_anchor_id.clone())
                .or_default()
                .insert(assertion.subject_anchor_id.clone())
        {
            *indegree
                .entry(assertion.subject_anchor_id.clone())
                .or_default() += 1;
        }
    }
    let mut ready = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(anchor, _)| anchor.clone())
        .collect::<BTreeSet<_>>();
    let mut ordered = Vec::with_capacity(by_anchor.len());
    while let Some(anchor) = ready.pop_first() {
        checkpoint(control, hook, ResolutionCheckpoint::Evolution)?;
        if let Some(item) = by_anchor.remove(&anchor) {
            ordered.push(item);
        }
        if let Some(children) = descendants.get(&anchor) {
            for child in children {
                checkpoint(control, hook, ResolutionCheckpoint::Evolution)?;
                if let Some(degree) = indegree.get_mut(child) {
                    *degree -= 1;
                    if *degree == 0 {
                        ready.insert(child.clone());
                    }
                }
            }
        }
    }
    let remaining_ids = by_anchor.keys().cloned().collect::<BTreeSet<_>>();
    let cycle_members = cycle_members_among(&remaining_ids, &descendants, control, hook)?;
    let mut cyclic_items = Vec::new();
    let mut blocked = BTreeMap::new();
    for (anchor_id, mut item) in by_anchor {
        checkpoint(control, hook, ResolutionCheckpoint::Evolution)?;
        if cycle_members.contains(&anchor_id) {
            item.conflicted = true;
            cyclic_items.push(item);
        } else {
            blocked.insert(anchor_id, item);
        }
    }
    cyclic_items.sort_by(stable_occurrence_order);
    ordered.extend(cyclic_items);

    // Condensation: cycle members are already emitted, so only edges among
    // blocked nodes continue to constrain topological order.
    let mut blocked_indegree = blocked
        .keys()
        .cloned()
        .map(|anchor| (anchor, 0_usize))
        .collect::<BTreeMap<_, _>>();
    for (parent, children) in &descendants {
        checkpoint(control, hook, ResolutionCheckpoint::Evolution)?;
        if !blocked.contains_key(parent) {
            continue;
        }
        for child in children {
            if blocked.contains_key(child)
                && let Some(degree) = blocked_indegree.get_mut(child)
            {
                *degree += 1;
            }
        }
    }
    let mut blocked_ready = blocked_indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(anchor, _)| anchor.clone())
        .collect::<BTreeSet<_>>();
    while let Some(anchor) = blocked_ready.pop_first() {
        checkpoint(control, hook, ResolutionCheckpoint::Evolution)?;
        if let Some(item) = blocked.remove(&anchor) {
            ordered.push(item);
        }
        if let Some(children) = descendants.get(&anchor) {
            for child in children {
                checkpoint(control, hook, ResolutionCheckpoint::Evolution)?;
                if let Some(degree) = blocked_indegree.get_mut(child) {
                    *degree = degree.saturating_sub(1);
                    if *degree == 0 {
                        blocked_ready.insert(child.clone());
                    }
                }
            }
        }
    }
    let mut leftover = blocked.into_values().collect::<Vec<_>>();
    leftover.sort_by(stable_occurrence_order);
    ordered.extend(leftover);
    Ok(ordered)
}

fn cycle_members_among(
    nodes: &BTreeSet<RetrievalAnchorId>,
    descendants: &BTreeMap<RetrievalAnchorId, BTreeSet<RetrievalAnchorId>>,
    control: &ExecutionControl,
    hook: &mut dyn FnMut(ResolutionCheckpoint) -> Result<(), TemporalPortError>,
) -> Result<BTreeSet<RetrievalAnchorId>, TemporalPortError> {
    let mut cyclic = BTreeSet::new();
    for start in nodes {
        checkpoint(control, hook, ResolutionCheckpoint::Evolution)?;
        if node_reaches_self(start, nodes, descendants, control, hook)? {
            cyclic.insert(start.clone());
        }
    }
    Ok(cyclic)
}

fn node_reaches_self(
    start: &RetrievalAnchorId,
    nodes: &BTreeSet<RetrievalAnchorId>,
    descendants: &BTreeMap<RetrievalAnchorId, BTreeSet<RetrievalAnchorId>>,
    control: &ExecutionControl,
    hook: &mut dyn FnMut(ResolutionCheckpoint) -> Result<(), TemporalPortError>,
) -> Result<bool, TemporalPortError> {
    let Some(seed) = descendants.get(start) else {
        return Ok(false);
    };
    let mut stack = seed
        .iter()
        .filter(|child| nodes.contains(child))
        .cloned()
        .collect::<Vec<_>>();
    let mut visited = BTreeSet::from([start.clone()]);
    while let Some(node) = stack.pop() {
        checkpoint(control, hook, ResolutionCheckpoint::Evolution)?;
        if &node == start {
            return Ok(true);
        }
        if !visited.insert(node.clone()) {
            continue;
        }
        if let Some(children) = descendants.get(&node) {
            for child in children {
                checkpoint(control, hook, ResolutionCheckpoint::Evolution)?;
                if nodes.contains(child) {
                    stack.push(child.clone());
                }
            }
        }
    }
    Ok(false)
}

fn copy_sources(
    copies: &[LogicalCopyRecordV1],
    control: &ExecutionControl,
    hook: &mut dyn FnMut(ResolutionCheckpoint) -> Result<(), TemporalPortError>,
) -> Result<BTreeMap<MessageOccurrenceIdV1, MessageOccurrenceIdV1>, TemporalPortError> {
    let mut validated = Vec::with_capacity(copies.len());
    for copy in copies {
        checkpoint(control, hook, ResolutionCheckpoint::Copy)?;
        if copy.validate().is_ok() {
            validated.push(copy);
        }
    }
    validated.sort_by(|left, right| {
        left.occurrence_id.cmp(&right.occurrence_id).then_with(|| {
            left.copied_from_occurrence_id
                .cmp(&right.copied_from_occurrence_id)
        })
    });
    let mut sources = BTreeMap::new();
    for copy in validated {
        checkpoint(control, hook, ResolutionCheckpoint::Copy)?;
        sources
            .entry(copy.occurrence_id.clone())
            .or_insert_with(|| copy.copied_from_occurrence_id.clone());
    }
    Ok(sources)
}

fn copy_root(
    occurrence_id: &MessageOccurrenceIdV1,
    sources: &BTreeMap<MessageOccurrenceIdV1, MessageOccurrenceIdV1>,
    eligible_ids: &BTreeSet<MessageOccurrenceIdV1>,
    control: &ExecutionControl,
    hook: &mut dyn FnMut(ResolutionCheckpoint) -> Result<(), TemporalPortError>,
) -> Result<MessageOccurrenceIdV1, TemporalPortError> {
    let mut current = occurrence_id.clone();
    let mut visited = BTreeSet::new();
    while visited.insert(current.clone()) {
        checkpoint(control, hook, ResolutionCheckpoint::Copy)?;
        let Some(parent) = sources.get(&current) else {
            break;
        };
        if !eligible_ids.contains(parent) {
            break;
        }
        current = parent.clone();
    }
    Ok(current)
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub enum SummarySourceState {
    Covered {
        knowledge_at: UtcMicros,
        valid_time: TemporalValidityV1,
    },
    Stale,
    Deleted,
    Redacted,
    Missing,
    Unauthorized,
    Locked,
    Expired,
    Unavailable,
    Cycle,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub enum SummaryLineageRejection {
    SessionMismatch,
    CreatedAfterCutoff,
    HorizonAfterCutoff,
    MissingValidHorizon,
    StaleSource {
        anchor_id: RetrievalAnchorId,
    },
    DeletedSource {
        anchor_id: RetrievalAnchorId,
    },
    RedactedSource {
        anchor_id: RetrievalAnchorId,
    },
    MissingSource {
        anchor_id: RetrievalAnchorId,
    },
    UnauthorizedSource {
        anchor_id: RetrievalAnchorId,
    },
    LockedSource {
        anchor_id: RetrievalAnchorId,
    },
    ExpiredSource {
        anchor_id: RetrievalAnchorId,
    },
    UnavailableSource {
        anchor_id: RetrievalAnchorId,
    },
    CycleSource {
        anchor_id: RetrievalAnchorId,
    },
    SourceBeyondKnowledgeHorizon {
        anchor_id: RetrievalAnchorId,
    },
    UnknownSourceValidTime {
        anchor_id: RetrievalAnchorId,
    },
    SourceBeyondValidHorizon {
        anchor_id: RetrievalAnchorId,
    },
    MissingPredecessor {
        predecessor_summary_id: SessionSummaryIdV1,
    },
    IneligiblePredecessor {
        predecessor_summary_id: SessionSummaryIdV1,
    },
    HorizonRegression {
        predecessor_summary_id: SessionSummaryIdV1,
    },
    Cycle,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SummaryOmission {
    pub summary_id: SessionSummaryIdV1,
    pub anchor_id: RetrievalAnchorId,
    pub rejection: SummaryLineageRejection,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SummaryLineageEligibility {
    pub eligible_anchor_ids: BTreeSet<RetrievalAnchorId>,
    pub suppressed_summary_ids: BTreeSet<SessionSummaryIdV1>,
    pub rejections: BTreeMap<SessionSummaryIdV1, SummaryLineageRejection>,
    pub omissions: Vec<SummaryOmission>,
}

pub fn evaluate_summary_lineage_eligibility(
    summaries: &[SessionSummaryRecordV1],
    source_states: &BTreeMap<RetrievalAnchorId, SummarySourceState>,
    session_id: &SessionId,
    mode: TemporalModeV1,
) -> Result<SummaryLineageEligibility, TemporalPortError> {
    evaluate_summary_lineage_eligibility_controlled(
        summaries,
        source_states,
        session_id,
        mode,
        &ExecutionControl::default(),
    )
}

pub fn evaluate_summary_lineage_eligibility_controlled(
    summaries: &[SessionSummaryRecordV1],
    source_states: &BTreeMap<RetrievalAnchorId, SummarySourceState>,
    session_id: &SessionId,
    mode: TemporalModeV1,
    control: &ExecutionControl,
) -> Result<SummaryLineageEligibility, TemporalPortError> {
    let by_id = summaries
        .iter()
        .map(|summary| (summary.summary_id().clone(), summary))
        .collect::<BTreeMap<_, _>>();
    let mut local_rejections = BTreeMap::new();
    for summary in summaries {
        control.checkpoint()?;
        if let Some(rejection) =
            summary_source_rejection(summary, source_states, session_id, mode, control)?
        {
            local_rejections.insert(summary.summary_id().clone(), rejection);
        }
    }
    let mut rejections = local_rejections.clone();

    for summary in summaries {
        control.checkpoint()?;
        if local_rejections.contains_key(summary.summary_id()) {
            continue;
        }
        if let Some(rejection) =
            summary_chain_rejection(summary, &by_id, &local_rejections, control)?
        {
            rejections.insert(summary.summary_id().clone(), rejection);
        }
    }

    let eligible_ids = summaries
        .iter()
        .filter(|summary| !rejections.contains_key(summary.summary_id()))
        .map(|summary| summary.summary_id().clone())
        .collect::<BTreeSet<_>>();
    let mut suppressed_summary_ids = BTreeSet::new();
    if mode == TemporalModeV1::Current {
        for summary in summaries {
            control.checkpoint()?;
            if !eligible_ids.contains(summary.summary_id()) {
                continue;
            }
            if let Some(predecessor_id) = summary.predecessor_summary_id()
                && eligible_ids.contains(predecessor_id)
            {
                suppressed_summary_ids.insert(predecessor_id.clone());
            }
        }
    }
    let eligible_anchor_ids = summaries
        .iter()
        .filter(|summary| {
            eligible_ids.contains(summary.summary_id())
                && !suppressed_summary_ids.contains(summary.summary_id())
        })
        .map(|summary| summary.summary_anchor_id().clone())
        .collect();
    let omissions = summaries
        .iter()
        .filter_map(|summary| {
            rejections
                .get(summary.summary_id())
                .cloned()
                .map(|rejection| SummaryOmission {
                    summary_id: summary.summary_id().clone(),
                    anchor_id: summary.summary_anchor_id().clone(),
                    rejection,
                })
        })
        .collect();

    Ok(SummaryLineageEligibility {
        eligible_anchor_ids,
        suppressed_summary_ids,
        rejections,
        omissions,
    })
}

fn summary_source_rejection(
    summary: &SessionSummaryRecordV1,
    source_states: &BTreeMap<RetrievalAnchorId, SummarySourceState>,
    session_id: &SessionId,
    mode: TemporalModeV1,
    control: &ExecutionControl,
) -> Result<Option<SummaryLineageRejection>, TemporalPortError> {
    if summary.session_id() != session_id {
        return Ok(Some(SummaryLineageRejection::SessionMismatch));
    }
    let horizon = summary.source_horizon();
    if let TemporalModeV1::AsOf { cutoff } = mode
        && summary.created_at() > cutoff
    {
        return Ok(Some(SummaryLineageRejection::CreatedAfterCutoff));
    }
    let Some(valid_through) = horizon.valid_through else {
        return Ok(Some(SummaryLineageRejection::MissingValidHorizon));
    };
    if let TemporalModeV1::AsOf { cutoff } = mode
        && (horizon.knowledge_through > cutoff || valid_through > cutoff)
    {
        return Ok(Some(SummaryLineageRejection::HorizonAfterCutoff));
    }
    for anchor_id in summary.source_anchors() {
        control.checkpoint()?;
        let state = source_states
            .get(anchor_id)
            .copied()
            .unwrap_or(SummarySourceState::Missing);
        match state {
            SummarySourceState::Covered {
                knowledge_at,
                valid_time,
            } => {
                if knowledge_at > horizon.knowledge_through {
                    return Ok(Some(
                        SummaryLineageRejection::SourceBeyondKnowledgeHorizon {
                            anchor_id: anchor_id.clone(),
                        },
                    ));
                }
                match valid_time {
                    TemporalValidityV1::Known { valid_at } if valid_at <= valid_through => {}
                    TemporalValidityV1::Known { .. } => {
                        return Ok(Some(SummaryLineageRejection::SourceBeyondValidHorizon {
                            anchor_id: anchor_id.clone(),
                        }));
                    }
                    TemporalValidityV1::Unknown => {
                        return Ok(Some(SummaryLineageRejection::UnknownSourceValidTime {
                            anchor_id: anchor_id.clone(),
                        }));
                    }
                }
            }
            SummarySourceState::Stale => {
                return Ok(Some(SummaryLineageRejection::StaleSource {
                    anchor_id: anchor_id.clone(),
                }));
            }
            SummarySourceState::Deleted => {
                return Ok(Some(SummaryLineageRejection::DeletedSource {
                    anchor_id: anchor_id.clone(),
                }));
            }
            SummarySourceState::Redacted => {
                return Ok(Some(SummaryLineageRejection::RedactedSource {
                    anchor_id: anchor_id.clone(),
                }));
            }
            SummarySourceState::Missing => {
                return Ok(Some(SummaryLineageRejection::MissingSource {
                    anchor_id: anchor_id.clone(),
                }));
            }
            SummarySourceState::Unauthorized => {
                return Ok(Some(SummaryLineageRejection::UnauthorizedSource {
                    anchor_id: anchor_id.clone(),
                }));
            }
            SummarySourceState::Locked => {
                return Ok(Some(SummaryLineageRejection::LockedSource {
                    anchor_id: anchor_id.clone(),
                }));
            }
            SummarySourceState::Expired => {
                return Ok(Some(SummaryLineageRejection::ExpiredSource {
                    anchor_id: anchor_id.clone(),
                }));
            }
            SummarySourceState::Unavailable => {
                return Ok(Some(SummaryLineageRejection::UnavailableSource {
                    anchor_id: anchor_id.clone(),
                }));
            }
            SummarySourceState::Cycle => {
                return Ok(Some(SummaryLineageRejection::CycleSource {
                    anchor_id: anchor_id.clone(),
                }));
            }
        }
    }
    Ok(None)
}

fn summary_chain_rejection(
    summary: &SessionSummaryRecordV1,
    by_id: &BTreeMap<SessionSummaryIdV1, &SessionSummaryRecordV1>,
    local_rejections: &BTreeMap<SessionSummaryIdV1, SummaryLineageRejection>,
    control: &ExecutionControl,
) -> Result<Option<SummaryLineageRejection>, TemporalPortError> {
    let mut cycle_cursor = summary;
    let mut cycle_visited = BTreeSet::from([summary.summary_id().clone()]);
    while let Some(predecessor_id) = cycle_cursor.predecessor_summary_id() {
        control.checkpoint()?;
        if !cycle_visited.insert(predecessor_id.clone()) {
            return Ok(Some(SummaryLineageRejection::Cycle));
        }
        let Some(predecessor) = by_id.get(predecessor_id).copied() else {
            break;
        };
        cycle_cursor = predecessor;
    }

    let mut cursor = summary;
    let mut visited = BTreeSet::from([summary.summary_id().clone()]);
    while let Some(predecessor_id) = cursor.predecessor_summary_id() {
        control.checkpoint()?;
        if !visited.insert(predecessor_id.clone()) {
            return Ok(Some(SummaryLineageRejection::Cycle));
        }
        let Some(predecessor) = by_id.get(predecessor_id).copied() else {
            return Ok(Some(SummaryLineageRejection::MissingPredecessor {
                predecessor_summary_id: predecessor_id.clone(),
            }));
        };
        if local_rejections.contains_key(predecessor_id) {
            return Ok(Some(SummaryLineageRejection::IneligiblePredecessor {
                predecessor_summary_id: predecessor_id.clone(),
            }));
        }
        let predecessor_horizon = predecessor.source_horizon();
        let cursor_horizon = cursor.source_horizon();
        if predecessor_horizon.knowledge_through > cursor_horizon.knowledge_through
            || predecessor_horizon.valid_through > cursor_horizon.valid_through
        {
            return Ok(Some(SummaryLineageRejection::HorizonRegression {
                predecessor_summary_id: predecessor_id.clone(),
            }));
        }
        cursor = predecessor;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::{
        CopyProofV1, LogicalCopyRecordV1, MessageOccurrenceIdV1, ObservationId, RetrievalAnchorId,
        SessionAuthorityClassV1, SessionId, SessionSummaryIdV1, SessionSummaryRecordV1,
        SummarySourceHorizonV1, TemporalAssertionKindV1, TemporalAssertionRecordV1, TemporalModeV1,
        TemporalValidityV1, UtcMicros,
    };

    use super::*;

    fn occurrence_id(byte: char) -> MessageOccurrenceIdV1 {
        MessageOccurrenceIdV1::new(format!("sha256:{}", byte.to_string().repeat(64)))
            .expect("valid occurrence id")
    }

    fn anchor(value: &str) -> RetrievalAnchorId {
        serde_json::from_str(&format!("\"{value}\"")).expect("valid anchor")
    }

    fn occurrence(
        id: char,
        anchor_id: &str,
        knowledge_at: i64,
        valid_time: TemporalValidityV1,
    ) -> ResolutionOccurrence {
        ResolutionOccurrence {
            occurrence_id: occurrence_id(id),
            anchor_id: anchor(anchor_id),
            knowledge_at: UtcMicros(knowledge_at),
            valid_time,
            evidence: ResolutionEvidence::new(
                SessionAuthorityClassV1::CanonicalObservation,
                ValidatedAuthorization::Authorized,
            ),
        }
    }

    fn assertion(
        kind: TemporalAssertionKindV1,
        subject: &str,
        object: &str,
        knowledge_at: i64,
    ) -> ResolutionAssertion {
        ResolutionAssertion {
            kind,
            subject_anchor_id: anchor(subject),
            object_anchor_id: anchor(object),
            knowledge_at: UtcMicros(knowledge_at),
            valid_time: TemporalValidityV1::Known {
                valid_at: UtcMicros(knowledge_at),
            },
            evidence: ResolutionEvidence::new(
                SessionAuthorityClassV1::CanonicalObservation,
                ValidatedAuthorization::Authorized,
            ),
        }
    }

    fn summary(
        id: &str,
        anchor_id: &str,
        source_anchor: &str,
        knowledge_through: i64,
        valid_through: i64,
    ) -> SessionSummaryRecordV1 {
        summary_with_sources(
            id,
            anchor_id,
            &[source_anchor],
            knowledge_through,
            valid_through,
        )
    }

    fn summary_with_sources(
        id: &str,
        anchor_id: &str,
        source_anchors: &[&str],
        knowledge_through: i64,
        valid_through: i64,
    ) -> SessionSummaryRecordV1 {
        let session_id: SessionId =
            serde_json::from_str("\"session-1\"").expect("valid session id");
        SessionSummaryRecordV1::new(
            SessionSummaryIdV1::new(id).expect("valid summary id"),
            session_id,
            anchor(anchor_id),
            source_anchors
                .iter()
                .map(|source_anchor| anchor(source_anchor))
                .collect(),
            SummarySourceHorizonV1 {
                knowledge_through: UtcMicros(knowledge_through),
                valid_through: Some(UtcMicros(valid_through)),
            },
            UtcMicros(knowledge_through),
        )
        .expect("valid summary")
    }

    fn covered_source(knowledge_at: i64, valid_at: i64) -> SummarySourceState {
        SummarySourceState::Covered {
            knowledge_at: UtcMicros(knowledge_at),
            valid_time: TemporalValidityV1::Known {
                valid_at: UtcMicros(valid_at),
            },
        }
    }

    #[test]
    fn only_explicit_copy_evidence_collapses_repetitions() {
        let first = occurrence(
            'a',
            "a",
            1,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(1),
            },
        );
        let copied = occurrence(
            'b',
            "b",
            2,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(2),
            },
        );
        let independent = occurrence(
            'c',
            "c",
            2,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(2),
            },
        );
        let provider_record_id: ObservationId =
            serde_json::from_str("\"provider-record\"").expect("valid observation id");
        let copy = LogicalCopyRecordV1 {
            occurrence_id: copied.occurrence_id.clone(),
            copied_from_occurrence_id: first.occurrence_id.clone(),
            proof: CopyProofV1::ProviderLinkage {
                source_occurrence_id: first.occurrence_id.clone(),
                provider_record_id,
            },
        };

        let resolved = resolve_temporal(
            &[first, copied, independent],
            &[copy],
            &[],
            TemporalModeV1::Current,
        )
        .expect("resolution succeeds");

        assert_eq!(resolved.len(), 2);
        assert!(
            resolved
                .iter()
                .any(|item| item.occurrence.anchor_id == anchor("a"))
        );
        assert!(
            resolved
                .iter()
                .any(|item| item.occurrence.anchor_id == anchor("c"))
        );
    }

    #[test]
    fn as_of_requires_known_valid_and_knowledge_time() {
        let resolved = resolve_temporal(
            &[
                occurrence(
                    'a',
                    "known",
                    5,
                    TemporalValidityV1::Known {
                        valid_at: UtcMicros(4),
                    },
                ),
                occurrence('b', "unknown", 3, TemporalValidityV1::Unknown),
                occurrence(
                    'c',
                    "late",
                    7,
                    TemporalValidityV1::Known {
                        valid_at: UtcMicros(3),
                    },
                ),
            ],
            &[],
            &[],
            TemporalModeV1::AsOf {
                cutoff: UtcMicros(5),
            },
        )
        .expect("resolution succeeds");

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].occurrence.anchor_id, anchor("known"));
    }

    #[test]
    fn current_applies_corrections_and_exposes_conflicts() {
        let original = occurrence(
            'a',
            "original",
            1,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(1),
            },
        );
        let correction = occurrence(
            'b',
            "correction",
            2,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(2),
            },
        );
        let rival = occurrence(
            'c',
            "rival",
            2,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(2),
            },
        );
        let assertions = [
            ResolutionAssertion {
                kind: TemporalAssertionKindV1::Corrects,
                subject_anchor_id: correction.anchor_id.clone(),
                object_anchor_id: original.anchor_id.clone(),
                knowledge_at: UtcMicros(2),
                valid_time: TemporalValidityV1::Known {
                    valid_at: UtcMicros(2),
                },
                evidence: ResolutionEvidence::new(
                    SessionAuthorityClassV1::CanonicalObservation,
                    ValidatedAuthorization::Authorized,
                ),
            },
            ResolutionAssertion {
                kind: TemporalAssertionKindV1::Contradicts,
                subject_anchor_id: correction.anchor_id.clone(),
                object_anchor_id: rival.anchor_id.clone(),
                knowledge_at: UtcMicros(2),
                valid_time: TemporalValidityV1::Known {
                    valid_at: UtcMicros(2),
                },
                evidence: ResolutionEvidence::new(
                    SessionAuthorityClassV1::CanonicalObservation,
                    ValidatedAuthorization::Authorized,
                ),
            },
        ];

        let resolved = resolve_temporal(
            &[original, correction, rival],
            &[],
            &assertions,
            TemporalModeV1::Current,
        )
        .expect("resolution succeeds");

        assert_eq!(resolved.len(), 2);
        assert!(resolved.iter().all(|item| item.conflicted));
        assert!(
            !resolved
                .iter()
                .any(|item| item.occurrence.anchor_id == anchor("original"))
        );
    }

    #[test]
    fn forensic_retains_uncertain_copies_in_stable_order() {
        let first = occurrence('a', "a", 2, TemporalValidityV1::Unknown);
        let copied = occurrence('b', "b", 1, TemporalValidityV1::Unknown);
        let mut unauthorized = occurrence('c', "denied", 0, TemporalValidityV1::Unknown);
        unauthorized.evidence = ResolutionEvidence::new(
            SessionAuthorityClassV1::CanonicalObservation,
            ValidatedAuthorization::Unauthorized,
        );

        let resolved = resolve_temporal(
            &[first, copied, unauthorized],
            &[],
            &[],
            TemporalModeV1::Forensic,
        )
        .expect("resolution succeeds");

        assert_eq!(resolved.len(), 2);
        assert!(resolved.iter().all(|item| item.uncertain));
        assert_eq!(resolved[0].occurrence.anchor_id, anchor("b"));
        assert_eq!(resolved[1].occurrence.anchor_id, anchor("a"));
    }

    #[test]
    fn current_does_not_let_unsupported_correction_erase_supported_evidence() {
        let mut original = occurrence(
            'a',
            "original",
            1,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(1),
            },
        );
        original.evidence.authority = SessionAuthorityClassV1::ProviderNative;
        let mut correction = occurrence(
            'b',
            "correction",
            2,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(2),
            },
        );
        correction.evidence.authority = SessionAuthorityClassV1::DerivedProjection;
        let witness = occurrence(
            'c',
            "witness",
            3,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(3),
            },
        );
        let mut assertions = [
            assertion(TemporalAssertionKindV1::Supports, "witness", "original", 3),
            assertion(
                TemporalAssertionKindV1::Corrects,
                "correction",
                "original",
                2,
            ),
        ];
        assertions[1].evidence.authority = SessionAuthorityClassV1::DerivedProjection;

        let resolved = resolve_temporal(
            &[original, correction, witness],
            &[],
            &assertions,
            TemporalModeV1::Current,
        )
        .expect("resolution succeeds");

        assert!(
            resolved
                .iter()
                .any(|item| item.occurrence.anchor_id == anchor("original"))
        );
        assert!(
            resolved
                .iter()
                .find(|item| item.occurrence.anchor_id == anchor("original"))
                .is_some_and(|item| item.supporting_anchor_ids.contains(&anchor("witness")))
        );
    }

    #[test]
    fn current_conflict_precedence_retains_the_authoritative_side() {
        let mut authoritative = occurrence(
            'a',
            "authoritative",
            1,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(1),
            },
        );
        authoritative.evidence.authority = SessionAuthorityClassV1::ProviderNative;
        let mut weak = occurrence(
            'b',
            "weak",
            2,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(2),
            },
        );
        weak.evidence.authority = SessionAuthorityClassV1::DerivedProjection;
        let mut contradiction = assertion(
            TemporalAssertionKindV1::Contradicts,
            "authoritative",
            "weak",
            3,
        );
        contradiction.evidence.authority = SessionAuthorityClassV1::ProviderNative;

        let resolved = resolve_temporal(
            &[authoritative, weak],
            &[],
            &[contradiction],
            TemporalModeV1::Current,
        )
        .expect("resolution succeeds");

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].occurrence.anchor_id, anchor("authoritative"));
        assert!(!resolved[0].conflicted);
    }

    #[test]
    fn evolution_orders_the_correction_chain_not_incidental_timestamps() {
        let original = occurrence(
            'a',
            "original",
            30,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(1),
            },
        );
        let correction = occurrence(
            'b',
            "correction",
            20,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(2),
            },
        );
        let superseding = occurrence(
            'c',
            "superseding",
            10,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(3),
            },
        );
        let assertions = [
            assertion(
                TemporalAssertionKindV1::Corrects,
                "correction",
                "original",
                31,
            ),
            assertion(
                TemporalAssertionKindV1::Supersedes,
                "superseding",
                "correction",
                32,
            ),
        ];

        let resolved = resolve_temporal(
            &[original, correction, superseding],
            &[],
            &assertions,
            TemporalModeV1::Evolution,
        )
        .expect("resolution succeeds");

        assert_eq!(
            resolved
                .iter()
                .map(|item| item.occurrence.anchor_id.clone())
                .collect::<Vec<_>>(),
            vec![
                anchor("original"),
                anchor("correction"),
                anchor("superseding")
            ]
        );
    }

    #[test]
    fn resolution_checks_live_work_budget_during_occurrence_consumption() {
        let occurrences = [
            occurrence('a', "a", 1, TemporalValidityV1::Unknown),
            occurrence('b', "b", 2, TemporalValidityV1::Unknown),
        ];
        let control = ExecutionControl::default().with_work_limit(1);

        assert_eq!(
            resolve_temporal_controlled(&occurrences, &[], &[], TemporalModeV1::Forensic, &control,),
            Err(TemporalPortError::BudgetExceeded {
                resource: "work units"
            })
        );
    }

    #[test]
    fn weak_correction_cannot_erase_authoritative_current_evidence() {
        let mut original = occurrence(
            'a',
            "original",
            1,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(1),
            },
        );
        original.evidence.authority = SessionAuthorityClassV1::ProviderNative;
        let mut correction = occurrence(
            'b',
            "correction",
            2,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(2),
            },
        );
        correction.evidence.authority = SessionAuthorityClassV1::DerivedProjection;
        let mut correction_edge = assertion(
            TemporalAssertionKindV1::Corrects,
            "correction",
            "original",
            2,
        );
        correction_edge.evidence.authority = SessionAuthorityClassV1::DerivedProjection;

        let resolved = resolve_temporal(
            &[original, correction],
            &[],
            &[correction_edge],
            TemporalModeV1::Current,
        )
        .expect("resolution succeeds");

        assert_eq!(resolved.occurrences.len(), 2);
        assert!(resolved.occurrences.iter().all(|item| item.conflicted));
    }

    #[test]
    fn strong_correction_suppresses_weaker_current_evidence() {
        let original = occurrence(
            'a',
            "original",
            1,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(1),
            },
        );
        let mut correction = occurrence(
            'b',
            "correction",
            2,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(2),
            },
        );
        correction.evidence.authority = SessionAuthorityClassV1::ProviderNative;
        let mut correction_edge = assertion(
            TemporalAssertionKindV1::Corrects,
            "correction",
            "original",
            2,
        );
        correction_edge.evidence.authority = SessionAuthorityClassV1::ProviderNative;

        let resolved = resolve_temporal(
            &[original, correction],
            &[],
            &[correction_edge],
            TemporalModeV1::Current,
        )
        .expect("resolution succeeds");

        assert_eq!(resolved.occurrences.len(), 1);
        assert_eq!(
            resolved.occurrences[0].occurrence.anchor_id,
            anchor("correction")
        );
    }

    #[test]
    fn unresolved_conflict_preserves_both_sides_and_a_typed_edge() {
        let left = occurrence(
            'a',
            "left",
            1,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(1),
            },
        );
        let right = occurrence(
            'b',
            "right",
            2,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(2),
            },
        );

        let resolved = resolve_temporal(
            &[left, right],
            &[],
            &[assertion(
                TemporalAssertionKindV1::Contradicts,
                "right",
                "left",
                3,
            )],
            TemporalModeV1::Current,
        )
        .expect("resolution succeeds");

        assert_eq!(resolved.occurrences.len(), 2);
        assert!(resolved.occurrences.iter().all(|item| item.conflicted));
        assert_eq!(resolved.lineage_edges.len(), 1);
        assert_eq!(
            resolved.lineage_edges[0].kind,
            ResolutionLineageEdgeKind::Contradiction
        );
    }

    #[test]
    fn evolution_returns_ordered_occurrences_and_typed_lineage_chain() {
        let original = occurrence(
            'a',
            "original",
            30,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(1),
            },
        );
        let correction = occurrence(
            'b',
            "correction",
            20,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(2),
            },
        );
        let successor = occurrence(
            'c',
            "successor",
            10,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(3),
            },
        );

        let resolved = resolve_temporal(
            &[original, correction, successor],
            &[],
            &[
                assertion(
                    TemporalAssertionKindV1::Corrects,
                    "correction",
                    "original",
                    31,
                ),
                assertion(
                    TemporalAssertionKindV1::Supersedes,
                    "successor",
                    "correction",
                    32,
                ),
            ],
            TemporalModeV1::Evolution,
        )
        .expect("resolution succeeds");

        assert_eq!(
            resolved
                .occurrences
                .iter()
                .map(|item| item.occurrence.anchor_id.clone())
                .collect::<Vec<_>>(),
            vec![
                anchor("original"),
                anchor("correction"),
                anchor("successor")
            ]
        );
        assert_eq!(
            resolved
                .lineage_edges
                .iter()
                .map(|edge| edge.kind)
                .collect::<Vec<_>>(),
            vec![
                ResolutionLineageEdgeKind::Correction,
                ResolutionLineageEdgeKind::Supersession,
            ]
        );
    }

    #[test]
    fn forensic_preserves_authorized_uncertainty_as_a_typed_state() {
        let unknown = occurrence('a', "unknown", 1, TemporalValidityV1::Unknown);
        let mut unauthorized = occurrence('b', "unauthorized", 2, TemporalValidityV1::Unknown);
        unauthorized.evidence = ResolutionEvidence::new(
            SessionAuthorityClassV1::CanonicalObservation,
            ValidatedAuthorization::Unauthorized,
        );

        let resolved =
            resolve_temporal(&[unknown, unauthorized], &[], &[], TemporalModeV1::Forensic)
                .expect("resolution succeeds");

        assert_eq!(resolved.occurrences.len(), 1);
        assert_eq!(
            resolved.occurrences[0].certainty(),
            ResolutionCertainty::AuthorizedUnknown
        );
    }

    #[test]
    fn cancellation_and_hook_budget_errors_propagate() {
        let input = [occurrence('a', "a", 1, TemporalValidityV1::Unknown)];
        let cancelled = ExecutionControl::default();
        cancelled.cancel();
        assert_eq!(
            resolve_temporal_controlled(&input, &[], &[], TemporalModeV1::Forensic, &cancelled,),
            Err(TemporalPortError::Cancelled)
        );

        let mut hook = |_checkpoint: ResolutionCheckpoint| {
            Err(TemporalPortError::BudgetExceeded {
                resource: "lineage traversal",
            })
        };
        assert_eq!(
            resolve_temporal_with_checkpoints(
                &input,
                &[],
                &[],
                TemporalModeV1::Forensic,
                &ExecutionControl::default(),
                &mut hook,
            ),
            Err(TemporalPortError::BudgetExceeded {
                resource: "lineage traversal",
            })
        );
    }

    #[test]
    fn unauthorized_assertion_conversion_fails_without_copying_lineage_metadata() {
        let record: TemporalAssertionRecordV1 = serde_json::from_str(
            r#"{
                "assertion_id":"assertion.explicit",
                "kind":"supports",
                "subject_anchor_id":"subject",
                "object_anchor_id":"object",
                "knowledge_at":10,
                "valid_time":{"kind":"known","valid_at":10},
                "evidence":{
                    "authority":"explicit_anchor_assertion",
                    "evidence_class":"provider_declared",
                    "source_anchor_id":"private-lineage",
                    "sanitization_receipt":{
                        "receipt_id":"receipt.explicit",
                        "sanitizer_version":"sanitizer.explicit"
                    }
                }
            }"#,
        )
        .expect("valid assertion fixture");

        assert_eq!(
            ResolutionAssertion::from_record(&record, ValidatedAuthorization::Unauthorized),
            Err(ResolutionInputError::UnauthorizedAssertion)
        );
    }

    #[test]
    fn summary_source_and_predecessor_traversal_preserve_control_errors() {
        let session_id: SessionId =
            serde_json::from_str("\"session-1\"").expect("valid session id");
        let predecessor = summary("predecessor", "old", "source-old", 5, 5);
        let successor = summary("successor", "new", "source-new", 6, 6)
            .with_predecessor(predecessor.summary_id().clone())
            .expect("valid predecessor");
        let states = [
            (anchor("source-old"), covered_source(5, 5)),
            (anchor("source-new"), covered_source(6, 6)),
        ]
        .into_iter()
        .collect();

        let cancelled = ExecutionControl::default();
        cancelled.cancel();
        assert_eq!(
            evaluate_summary_lineage_eligibility_controlled(
                &[predecessor.clone(), successor.clone()],
                &states,
                &session_id,
                TemporalModeV1::Current,
                &cancelled,
            ),
            Err(TemporalPortError::Cancelled)
        );

        let bounded = ExecutionControl::default().with_work_limit(6);
        assert_eq!(
            evaluate_summary_lineage_eligibility_controlled(
                &[predecessor, successor],
                &states,
                &session_id,
                TemporalModeV1::Current,
                &bounded,
            ),
            Err(TemporalPortError::BudgetExceeded {
                resource: "work units"
            })
        );
    }

    #[test]
    fn unrelated_newer_occurrence_does_not_stale_summary() {
        let session_id: SessionId =
            serde_json::from_str("\"session-1\"").expect("valid session id");
        let summaries = [summary("summary-a", "summary-a", "source-a", 7, 6)];
        let source_states = [
            (anchor("source-a"), covered_source(7, 6)),
            (anchor("unrelated"), covered_source(99, 99)),
        ]
        .into_iter()
        .collect();

        let eligibility = evaluate_summary_lineage_eligibility(
            &summaries,
            &source_states,
            &session_id,
            TemporalModeV1::Current,
        )
        .expect("eligibility");

        assert_eq!(
            eligibility.eligible_anchor_ids,
            [anchor("summary-a")].into_iter().collect()
        );
        assert!(eligibility.rejections.is_empty());
    }

    #[test]
    fn invalid_successor_does_not_suppress_eligible_predecessor() {
        let session_id: SessionId =
            serde_json::from_str("\"session-1\"").expect("valid session id");
        let predecessor = summary("predecessor", "summary-old", "source-old", 5, 5);
        let successor = summary("successor", "summary-new", "source-new", 7, 7)
            .with_predecessor(predecessor.summary_id().clone())
            .expect("valid predecessor");
        let source_states = [
            (anchor("source-old"), covered_source(5, 5)),
            (anchor("source-new"), SummarySourceState::Stale),
        ]
        .into_iter()
        .collect();

        let eligibility = evaluate_summary_lineage_eligibility(
            &[predecessor, successor],
            &source_states,
            &session_id,
            TemporalModeV1::Current,
        )
        .expect("eligibility");

        assert_eq!(
            eligibility.eligible_anchor_ids,
            [anchor("summary-old")].into_iter().collect()
        );
        assert!(eligibility.suppressed_summary_ids.is_empty());
        assert!(matches!(
            eligibility
                .rejections
                .get(&SessionSummaryIdV1::new("successor").expect("valid id")),
            Some(SummaryLineageRejection::StaleSource { .. })
        ));
    }

    #[test]
    fn summary_lineage_cycles_are_ineligible() {
        let session_id: SessionId =
            serde_json::from_str("\"session-1\"").expect("valid session id");
        let first = summary("first", "summary-first", "source-first", 5, 5)
            .with_predecessor(SessionSummaryIdV1::new("second").expect("valid id"))
            .expect("non-self predecessor");
        let second = summary("second", "summary-second", "source-second", 6, 6)
            .with_predecessor(SessionSummaryIdV1::new("first").expect("valid id"))
            .expect("non-self predecessor");
        let source_states = [
            (anchor("source-first"), covered_source(5, 5)),
            (anchor("source-second"), covered_source(6, 6)),
        ]
        .into_iter()
        .collect();

        let eligibility = evaluate_summary_lineage_eligibility(
            &[first, second],
            &source_states,
            &session_id,
            TemporalModeV1::Current,
        )
        .expect("eligibility");

        assert!(eligibility.eligible_anchor_ids.is_empty());
        assert_eq!(
            eligibility
                .rejections
                .values()
                .filter(|reason| matches!(reason, SummaryLineageRejection::Cycle))
                .count(),
            2
        );
    }

    #[test]
    fn source_specific_horizon_rejects_only_the_out_of_coverage_summary() {
        let session_id: SessionId =
            serde_json::from_str("\"session-1\"").expect("valid session id");
        let covered = summary("covered", "summary-covered", "covered-source", 7, 7);
        let stale_horizon = summary("stale-horizon", "summary-stale", "advanced-source", 7, 7);
        let source_states = [
            (anchor("covered-source"), covered_source(7, 7)),
            (anchor("advanced-source"), covered_source(8, 7)),
        ]
        .into_iter()
        .collect();

        let eligibility = evaluate_summary_lineage_eligibility(
            &[covered, stale_horizon],
            &source_states,
            &session_id,
            TemporalModeV1::Current,
        )
        .expect("eligibility");

        assert_eq!(
            eligibility.eligible_anchor_ids,
            [anchor("summary-covered")].into_iter().collect()
        );
        assert!(matches!(
            eligibility
                .rejections
                .get(&SessionSummaryIdV1::new("stale-horizon").expect("valid id")),
            Some(SummaryLineageRejection::SourceBeyondKnowledgeHorizon { .. })
        ));
    }

    #[test]
    fn all_summary_source_states_have_distinct_eligibility_or_rejections() {
        let session_id: SessionId =
            serde_json::from_str("\"session-1\"").expect("valid session id");
        let summaries = [
            summary("covered", "summary-covered", "covered-source", 7, 7),
            summary("stale", "summary-stale", "stale-source", 7, 7),
            summary("deleted", "summary-deleted", "deleted-source", 7, 7),
            summary("redacted", "summary-redacted", "redacted-source", 7, 7),
            summary("missing", "summary-missing", "missing-source", 7, 7),
            summary(
                "unauthorized",
                "summary-unauthorized",
                "unauthorized-source",
                7,
                7,
            ),
            summary("locked", "summary-locked", "locked-source", 7, 7),
            summary("expired", "summary-expired", "expired-source", 7, 7),
            summary(
                "unavailable",
                "summary-unavailable",
                "unavailable-source",
                7,
                7,
            ),
            summary("cycle-source", "summary-cycle", "cycle-source", 7, 7),
        ];
        let source_states = [
            (anchor("covered-source"), covered_source(7, 7)),
            (anchor("stale-source"), SummarySourceState::Stale),
            (anchor("deleted-source"), SummarySourceState::Deleted),
            (anchor("redacted-source"), SummarySourceState::Redacted),
            (anchor("missing-source"), SummarySourceState::Missing),
            (
                anchor("unauthorized-source"),
                SummarySourceState::Unauthorized,
            ),
            (anchor("locked-source"), SummarySourceState::Locked),
            (anchor("expired-source"), SummarySourceState::Expired),
            (
                anchor("unavailable-source"),
                SummarySourceState::Unavailable,
            ),
            (anchor("cycle-source"), SummarySourceState::Cycle),
        ]
        .into_iter()
        .collect();

        let eligibility = evaluate_summary_lineage_eligibility(
            &summaries,
            &source_states,
            &session_id,
            TemporalModeV1::Current,
        )
        .expect("eligibility");

        assert_eq!(
            eligibility.eligible_anchor_ids,
            [anchor("summary-covered")].into_iter().collect()
        );
        assert_eq!(eligibility.omissions.len(), 9);
        assert!(matches!(
            eligibility
                .rejections
                .get(&SessionSummaryIdV1::new("stale").expect("valid id")),
            Some(SummaryLineageRejection::StaleSource { .. })
        ));
        assert!(matches!(
            eligibility
                .rejections
                .get(&SessionSummaryIdV1::new("deleted").expect("valid id")),
            Some(SummaryLineageRejection::DeletedSource { .. })
        ));
        assert!(matches!(
            eligibility
                .rejections
                .get(&SessionSummaryIdV1::new("redacted").expect("valid id")),
            Some(SummaryLineageRejection::RedactedSource { .. })
        ));
        assert!(matches!(
            eligibility
                .rejections
                .get(&SessionSummaryIdV1::new("missing").expect("valid id")),
            Some(SummaryLineageRejection::MissingSource { .. })
        ));
        assert!(matches!(
            eligibility
                .rejections
                .get(&SessionSummaryIdV1::new("unauthorized").expect("valid id")),
            Some(SummaryLineageRejection::UnauthorizedSource { .. })
        ));
        assert!(matches!(
            eligibility
                .rejections
                .get(&SessionSummaryIdV1::new("locked").expect("valid id")),
            Some(SummaryLineageRejection::LockedSource { .. })
        ));
        assert!(matches!(
            eligibility
                .rejections
                .get(&SessionSummaryIdV1::new("expired").expect("valid id")),
            Some(SummaryLineageRejection::ExpiredSource { .. })
        ));
        assert!(matches!(
            eligibility
                .rejections
                .get(&SessionSummaryIdV1::new("unavailable").expect("valid id")),
            Some(SummaryLineageRejection::UnavailableSource { .. })
        ));
        assert!(matches!(
            eligibility
                .rejections
                .get(&SessionSummaryIdV1::new("cycle-source").expect("valid id")),
            Some(SummaryLineageRejection::CycleSource { .. })
        ));
    }

    #[test]
    fn unauthorized_and_session_mismatch_remain_lossless_and_distinct() {
        let summary = summary(
            "privacy-state",
            "summary-privacy-state",
            "source-privacy-state",
            7,
            7,
        );
        let source_states = [(
            anchor("source-privacy-state"),
            SummarySourceState::Unauthorized,
        )]
        .into_iter()
        .collect();
        let authorized_session: SessionId =
            serde_json::from_str("\"session-1\"").expect("valid session id");
        let mismatched_session: SessionId =
            serde_json::from_str("\"session-2\"").expect("valid session id");

        let unauthorized = evaluate_summary_lineage_eligibility(
            std::slice::from_ref(&summary),
            &source_states,
            &authorized_session,
            TemporalModeV1::Current,
        )
        .expect("unauthorized eligibility");
        let mismatched = evaluate_summary_lineage_eligibility(
            std::slice::from_ref(&summary),
            &source_states,
            &mismatched_session,
            TemporalModeV1::Current,
        )
        .expect("mismatched eligibility");

        assert_eq!(
            unauthorized.omissions,
            vec![SummaryOmission {
                summary_id: summary.summary_id().clone(),
                anchor_id: summary.summary_anchor_id().clone(),
                rejection: SummaryLineageRejection::UnauthorizedSource {
                    anchor_id: anchor("source-privacy-state"),
                },
            }]
        );
        assert_eq!(
            mismatched.omissions,
            vec![SummaryOmission {
                summary_id: summary.summary_id().clone(),
                anchor_id: summary.summary_anchor_id().clone(),
                rejection: SummaryLineageRejection::SessionMismatch,
            }]
        );
    }

    #[test]
    fn unauthorized_source_dominates_all_source_order_permutations() {
        let source_states = [
            (anchor("missing"), SummarySourceState::Missing),
            (anchor("redacted"), SummarySourceState::Redacted),
            (anchor("locked"), SummarySourceState::Locked),
            (anchor("expired"), SummarySourceState::Expired),
            (anchor("deleted"), SummarySourceState::Deleted),
            (anchor("unavailable"), SummarySourceState::Unavailable),
            (anchor("stale"), SummarySourceState::Stale),
            (anchor("unauthorized"), SummarySourceState::Unauthorized),
        ]
        .into_iter()
        .collect();
        let session_id = SessionId::new("session-1").expect("valid session id");
        let forward = [
            "missing",
            "redacted",
            "locked",
            "expired",
            "deleted",
            "unavailable",
            "stale",
            "unauthorized",
        ];
        let reverse = [
            "unauthorized",
            "stale",
            "unavailable",
            "deleted",
            "expired",
            "locked",
            "redacted",
            "missing",
        ];

        for source_anchors in [forward.as_slice(), reverse.as_slice()] {
            let summary =
                summary_with_sources("mixed", "summary-mixed", source_anchors, 7, 7);
            let eligibility = evaluate_summary_lineage_eligibility(
                std::slice::from_ref(&summary),
                &source_states,
                &session_id,
                TemporalModeV1::Current,
            )
            .expect("mixed-source eligibility");

            assert_eq!(
                eligibility
                    .rejections
                    .get(&SessionSummaryIdV1::new("mixed").expect("valid id")),
                Some(&SummaryLineageRejection::UnauthorizedSource {
                    anchor_id: anchor("unauthorized"),
                })
            );
        }
    }

    #[test]
    fn non_hidden_source_precedence_is_deterministic() {
        let cases = [
            (
                SummarySourceState::Redacted,
                SummarySourceState::Locked,
                SummaryLineageRejection::RedactedSource {
                    anchor_id: anchor("left"),
                },
            ),
            (
                SummarySourceState::Locked,
                SummarySourceState::Expired,
                SummaryLineageRejection::LockedSource {
                    anchor_id: anchor("left"),
                },
            ),
            (
                SummarySourceState::Expired,
                SummarySourceState::Deleted,
                SummaryLineageRejection::ExpiredSource {
                    anchor_id: anchor("left"),
                },
            ),
            (
                SummarySourceState::Deleted,
                SummarySourceState::Unavailable,
                SummaryLineageRejection::DeletedSource {
                    anchor_id: anchor("left"),
                },
            ),
            (
                SummarySourceState::Unavailable,
                SummarySourceState::Stale,
                SummaryLineageRejection::UnavailableSource {
                    anchor_id: anchor("left"),
                },
            ),
            (
                SummarySourceState::Stale,
                SummarySourceState::Missing,
                SummaryLineageRejection::StaleSource {
                    anchor_id: anchor("left"),
                },
            ),
        ];
        let session_id = SessionId::new("session-1").expect("valid session id");

        for (left, right, expected) in cases {
            for source_anchors in [["left", "right"], ["right", "left"]] {
                let summary =
                    summary_with_sources("precedence", "summary-precedence", &source_anchors, 7, 7);
                let source_states = [
                    (anchor("left"), left),
                    (anchor("right"), right),
                ]
                .into_iter()
                .collect();
                let eligibility = evaluate_summary_lineage_eligibility(
                    std::slice::from_ref(&summary),
                    &source_states,
                    &session_id,
                    TemporalModeV1::Current,
                )
                .expect("precedence eligibility");

                assert_eq!(
                    eligibility
                        .rejections
                        .get(&SessionSummaryIdV1::new("precedence").expect("valid id")),
                    Some(&expected)
                );
            }
        }
    }

    #[test]
    fn unauthorized_source_dominates_summary_horizon_failures() {
        let summary = summary(
            "horizon-private",
            "summary-horizon-private",
            "source-horizon-private",
            20,
            20,
        );
        let source_states = [(
            anchor("source-horizon-private"),
            SummarySourceState::Unauthorized,
        )]
        .into_iter()
        .collect();
        let session_id = SessionId::new("session-1").expect("valid session id");

        let eligibility = evaluate_summary_lineage_eligibility(
            std::slice::from_ref(&summary),
            &source_states,
            &session_id,
            TemporalModeV1::AsOf {
                cutoff: UtcMicros(10),
            },
        )
        .expect("private horizon eligibility");

        assert_eq!(
            eligibility
                .rejections
                .get(&SessionSummaryIdV1::new("horizon-private").expect("valid id")),
            Some(&SummaryLineageRejection::UnauthorizedSource {
                anchor_id: anchor("source-horizon-private"),
            })
        );
    }

    fn provider_copy(
        occurrence: &ResolutionOccurrence,
        source: &ResolutionOccurrence,
    ) -> LogicalCopyRecordV1 {
        let provider_record_id: ObservationId =
            serde_json::from_str("\"provider-record\"").expect("valid observation id");
        LogicalCopyRecordV1 {
            occurrence_id: occurrence.occurrence_id.clone(),
            copied_from_occurrence_id: source.occurrence_id.clone(),
            proof: CopyProofV1::ProviderLinkage {
                source_occurrence_id: source.occurrence_id.clone(),
                provider_record_id,
            },
        }
    }

    #[test]
    fn forensic_preserves_explicit_logical_copy_occurrences() {
        let first = occurrence(
            'a',
            "a",
            1,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(1),
            },
        );
        let copied = occurrence(
            'b',
            "b",
            2,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(2),
            },
        );
        let copy = provider_copy(&copied, &first);

        let resolved = resolve_temporal(&[first, copied], &[copy], &[], TemporalModeV1::Forensic)
            .expect("resolution succeeds");

        assert_eq!(resolved.len(), 2);
        assert!(
            resolved
                .iter()
                .any(|item| item.occurrence.anchor_id == anchor("a"))
        );
        assert!(
            resolved
                .iter()
                .any(|item| item.occurrence.anchor_id == anchor("b"))
        );
    }

    #[test]
    fn as_of_cutoff_is_inclusive_for_occurrences_and_assertions() {
        let boundary = occurrence(
            'a',
            "boundary",
            5,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(5),
            },
        );
        let late = occurrence(
            'b',
            "late",
            6,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(5),
            },
        );
        let witness = occurrence(
            'c',
            "witness",
            5,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(4),
            },
        );
        let support = assertion(TemporalAssertionKindV1::Supports, "witness", "boundary", 5);

        let resolved = resolve_temporal(
            &[boundary, late, witness],
            &[],
            &[support],
            TemporalModeV1::AsOf {
                cutoff: UtcMicros(5),
            },
        )
        .expect("resolution succeeds");

        assert!(
            resolved
                .iter()
                .any(|item| item.occurrence.anchor_id == anchor("boundary"))
        );
        assert!(
            !resolved
                .iter()
                .any(|item| item.occurrence.anchor_id == anchor("late"))
        );
        assert!(
            resolved
                .iter()
                .find(|item| item.occurrence.anchor_id == anchor("boundary"))
                .is_some_and(|item| item.supporting_anchor_ids.contains(&anchor("witness")))
        );
    }

    #[test]
    fn as_of_ignores_assertions_beyond_knowledge_or_valid_cutoff() {
        let original = occurrence(
            'a',
            "original",
            1,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(1),
            },
        );
        let correction = occurrence(
            'b',
            "correction",
            10,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(10),
            },
        );
        let late_edge = assertion(
            TemporalAssertionKindV1::Corrects,
            "correction",
            "original",
            10,
        );

        let resolved = resolve_temporal(
            &[original, correction],
            &[],
            &[late_edge],
            TemporalModeV1::AsOf {
                cutoff: UtcMicros(5),
            },
        )
        .expect("resolution succeeds");

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].occurrence.anchor_id, anchor("original"));
        assert!(resolved.lineage_edges.is_empty());
    }

    #[test]
    fn summary_as_of_enforces_created_and_source_horizon_cutoffs() {
        let session_id: SessionId =
            serde_json::from_str("\"session-1\"").expect("valid session id");
        let created_late = SessionSummaryRecordV1::new(
            SessionSummaryIdV1::new("created-late").expect("valid summary id"),
            session_id.clone(),
            anchor("summary-created"),
            vec![anchor("source-ok")],
            SummarySourceHorizonV1 {
                knowledge_through: UtcMicros(4),
                valid_through: Some(UtcMicros(4)),
            },
            UtcMicros(9),
        )
        .expect("valid summary");
        // Domain requires created_at >= knowledge_through, so a pure horizon breach uses
        // valid_through beyond cutoff while creation stays at/under the as-of bound.
        let horizon_late = SessionSummaryRecordV1::new(
            SessionSummaryIdV1::new("horizon-late").expect("valid summary id"),
            session_id.clone(),
            anchor("summary-horizon"),
            vec![anchor("source-ok")],
            SummarySourceHorizonV1 {
                knowledge_through: UtcMicros(4),
                valid_through: Some(UtcMicros(9)),
            },
            UtcMicros(4),
        )
        .expect("valid summary");
        let source_states = [(anchor("source-ok"), covered_source(4, 4))]
            .into_iter()
            .collect();

        let eligibility = evaluate_summary_lineage_eligibility(
            &[created_late, horizon_late],
            &source_states,
            &session_id,
            TemporalModeV1::AsOf {
                cutoff: UtcMicros(5),
            },
        )
        .expect("eligibility");

        assert!(eligibility.eligible_anchor_ids.is_empty());
        assert!(matches!(
            eligibility
                .rejections
                .get(&SessionSummaryIdV1::new("created-late").expect("valid id")),
            Some(SummaryLineageRejection::CreatedAfterCutoff)
        ));
        assert!(matches!(
            eligibility
                .rejections
                .get(&SessionSummaryIdV1::new("horizon-late").expect("valid id")),
            Some(SummaryLineageRejection::HorizonAfterCutoff)
        ));
    }

    #[test]
    fn as_of_missing_valid_horizon_is_reported_as_missing() {
        let session_id: SessionId =
            serde_json::from_str("\"session-1\"").expect("valid session id");
        let missing_horizon = SessionSummaryRecordV1::new(
            SessionSummaryIdV1::new("missing-horizon").expect("valid summary id"),
            session_id.clone(),
            anchor("summary-missing-horizon"),
            vec![anchor("source-ok")],
            SummarySourceHorizonV1 {
                knowledge_through: UtcMicros(4),
                valid_through: None,
            },
            UtcMicros(4),
        )
        .expect("valid summary");
        let source_states = [(anchor("source-ok"), covered_source(4, 4))]
            .into_iter()
            .collect();

        let eligibility = evaluate_summary_lineage_eligibility(
            &[missing_horizon],
            &source_states,
            &session_id,
            TemporalModeV1::AsOf {
                cutoff: UtcMicros(5),
            },
        )
        .expect("eligibility");

        assert!(matches!(
            eligibility
                .rejections
                .get(&SessionSummaryIdV1::new("missing-horizon").expect("valid id")),
            Some(SummaryLineageRejection::MissingValidHorizon)
        ));
    }

    #[test]
    fn current_suppresses_only_an_eligible_predecessor() {
        let session_id: SessionId =
            serde_json::from_str("\"session-1\"").expect("valid session id");
        let predecessor = summary("predecessor", "summary-old", "source-old", 5, 5);
        let successor = summary("successor", "summary-new", "source-new", 7, 7)
            .with_predecessor(predecessor.summary_id().clone())
            .expect("valid predecessor");
        let source_states = [
            (anchor("source-old"), covered_source(5, 5)),
            (anchor("source-new"), covered_source(7, 7)),
        ]
        .into_iter()
        .collect();

        let eligibility = evaluate_summary_lineage_eligibility(
            &[predecessor, successor],
            &source_states,
            &session_id,
            TemporalModeV1::Current,
        )
        .expect("eligibility");

        assert_eq!(
            eligibility.eligible_anchor_ids,
            [anchor("summary-new")].into_iter().collect()
        );
        assert_eq!(
            eligibility.suppressed_summary_ids,
            [SessionSummaryIdV1::new("predecessor").expect("valid id")]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn non_current_summary_modes_retain_eligible_predecessors() {
        let session_id: SessionId =
            serde_json::from_str("\"session-1\"").expect("valid session id");
        let predecessor = summary("predecessor", "summary-old", "source-old", 5, 5);
        let successor = summary("successor", "summary-new", "source-new", 7, 7)
            .with_predecessor(predecessor.summary_id().clone())
            .expect("valid predecessor");
        let source_states = [
            (anchor("source-old"), covered_source(5, 5)),
            (anchor("source-new"), covered_source(7, 7)),
        ]
        .into_iter()
        .collect();

        for mode in [TemporalModeV1::Evolution, TemporalModeV1::Forensic] {
            let eligibility = evaluate_summary_lineage_eligibility(
                &[predecessor.clone(), successor.clone()],
                &source_states,
                &session_id,
                mode,
            )
            .expect("eligibility");
            assert_eq!(
                eligibility.eligible_anchor_ids,
                [anchor("summary-old"), anchor("summary-new")]
                    .into_iter()
                    .collect(),
                "{mode:?} must retain eligible predecessor summaries"
            );
            assert!(eligibility.suppressed_summary_ids.is_empty());
        }
    }

    #[test]
    fn missing_and_unknown_validity_sources_have_distinct_rejections() {
        let session_id: SessionId =
            serde_json::from_str("\"session-1\"").expect("valid session id");
        let missing = summary("missing", "summary-missing", "missing-source", 7, 7);
        let unknown_valid = summary("unknown-valid", "summary-unknown", "unknown-source", 7, 7);
        let source_states = [
            (
                anchor("unknown-source"),
                SummarySourceState::Covered {
                    knowledge_at: UtcMicros(7),
                    valid_time: TemporalValidityV1::Unknown,
                },
            ),
            // missing-source intentionally absent from the map
        ]
        .into_iter()
        .collect();

        let eligibility = evaluate_summary_lineage_eligibility(
            &[missing, unknown_valid],
            &source_states,
            &session_id,
            TemporalModeV1::Current,
        )
        .expect("eligibility");

        assert!(matches!(
            eligibility
                .rejections
                .get(&SessionSummaryIdV1::new("missing").expect("valid id")),
            Some(SummaryLineageRejection::MissingSource { .. })
        ));
        assert!(matches!(
            eligibility
                .rejections
                .get(&SessionSummaryIdV1::new("unknown-valid").expect("valid id")),
            Some(SummaryLineageRejection::UnknownSourceValidTime { .. })
        ));
    }

    #[test]
    fn evolution_marks_only_cycle_members_conflicted() {
        let cycle_a = occurrence(
            'a',
            "cycle-a",
            1,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(1),
            },
        );
        let cycle_b = occurrence(
            'b',
            "cycle-b",
            2,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(2),
            },
        );
        let blocked = occurrence(
            'c',
            "blocked",
            3,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(3),
            },
        );
        let assertions = [
            assertion(TemporalAssertionKindV1::Corrects, "cycle-b", "cycle-a", 4),
            assertion(TemporalAssertionKindV1::Corrects, "cycle-a", "cycle-b", 5),
            assertion(TemporalAssertionKindV1::Supersedes, "blocked", "cycle-a", 6),
        ];

        let resolved = resolve_temporal(
            &[cycle_a, cycle_b, blocked],
            &[],
            &assertions,
            TemporalModeV1::Evolution,
        )
        .expect("resolution succeeds");

        let by_anchor = resolved
            .iter()
            .map(|item| (item.occurrence.anchor_id.clone(), item.conflicted))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(by_anchor.get(&anchor("cycle-a")), Some(&true));
        assert_eq!(by_anchor.get(&anchor("cycle-b")), Some(&true));
        assert_eq!(by_anchor.get(&anchor("blocked")), Some(&false));
        let order = resolved
            .iter()
            .map(|item| item.occurrence.anchor_id.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            order,
            vec![anchor("cycle-a"), anchor("cycle-b"), anchor("blocked")],
            "cycle SCC members must precede blocked descendants"
        );
    }

    #[test]
    fn current_correction_chain_keeps_only_the_tip() {
        let original = occurrence(
            'a',
            "original",
            1,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(1),
            },
        );
        let mid = occurrence(
            'b',
            "mid",
            2,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(2),
            },
        );
        let tip = occurrence(
            'c',
            "tip",
            3,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(3),
            },
        );
        let resolved = resolve_temporal(
            &[original, mid, tip],
            &[],
            &[
                assertion(TemporalAssertionKindV1::Corrects, "mid", "original", 2),
                assertion(TemporalAssertionKindV1::Corrects, "tip", "mid", 3),
            ],
            TemporalModeV1::Current,
        )
        .expect("resolution succeeds");

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].occurrence.anchor_id, anchor("tip"));
        assert!(!resolved[0].conflicted);
    }

    #[test]
    fn current_mutual_corrections_surface_conflict_instead_of_empty_set() {
        let left = occurrence(
            'a',
            "left",
            1,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(1),
            },
        );
        let right = occurrence(
            'b',
            "right",
            2,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(2),
            },
        );
        let resolved = resolve_temporal(
            &[left, right],
            &[],
            &[
                assertion(TemporalAssertionKindV1::Corrects, "right", "left", 3),
                assertion(TemporalAssertionKindV1::Corrects, "left", "right", 4),
            ],
            TemporalModeV1::Current,
        )
        .expect("resolution succeeds");

        assert_eq!(resolved.len(), 2);
        assert!(resolved.iter().all(|item| item.conflicted));
    }

    #[test]
    fn copy_root_does_not_traverse_ineligible_parents() {
        let mut root = occurrence(
            'a',
            "root",
            1,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(1),
            },
        );
        root.evidence = ResolutionEvidence::new(
            SessionAuthorityClassV1::CanonicalObservation,
            ValidatedAuthorization::Unauthorized,
        );
        let copied = occurrence(
            'b',
            "copied",
            2,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(2),
            },
        );
        let copy = provider_copy(&copied, &root);

        let resolved = resolve_temporal(&[root, copied], &[copy], &[], TemporalModeV1::Current)
            .expect("resolution succeeds");

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].occurrence.anchor_id, anchor("copied"));
        assert_eq!(
            resolved[0].representative_id,
            resolved[0].occurrence.occurrence_id
        );
    }

    #[test]
    fn evolution_lineage_edges_are_order_independent() {
        let original = occurrence(
            'a',
            "original",
            1,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(1),
            },
        );
        let left = occurrence(
            'b',
            "left",
            2,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(2),
            },
        );
        let right = occurrence(
            'c',
            "right",
            3,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(3),
            },
        );
        let mut forward = [
            assertion(TemporalAssertionKindV1::Corrects, "left", "original", 40),
            assertion(TemporalAssertionKindV1::Corrects, "right", "original", 30),
        ];
        let baseline = resolve_temporal(
            &[original.clone(), left.clone(), right.clone()],
            &[],
            &forward,
            TemporalModeV1::Evolution,
        )
        .expect("resolution succeeds");
        forward.reverse();
        let reversed = resolve_temporal(
            &[original, left, right],
            &[],
            &forward,
            TemporalModeV1::Evolution,
        )
        .expect("resolution succeeds");

        assert_eq!(baseline.lineage_edges, reversed.lineage_edges);
        assert_eq!(
            baseline
                .lineage_edges
                .iter()
                .map(|edge| (
                    edge.subject_anchor_id.clone(),
                    edge.object_anchor_id.clone(),
                    edge.knowledge_at
                ))
                .collect::<Vec<_>>(),
            vec![
                (anchor("left"), anchor("original"), UtcMicros(40)),
                (anchor("right"), anchor("original"), UtcMicros(30)),
            ]
        );
    }

    #[test]
    fn current_strong_supersession_suppresses_weaker_evidence() {
        let original = occurrence(
            'a',
            "original",
            1,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(1),
            },
        );
        let mut successor = occurrence(
            'b',
            "successor",
            2,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(2),
            },
        );
        successor.evidence.authority = SessionAuthorityClassV1::ProviderNative;
        let mut edge = assertion(
            TemporalAssertionKindV1::Supersedes,
            "successor",
            "original",
            2,
        );
        edge.evidence.authority = SessionAuthorityClassV1::ProviderNative;

        let resolved = resolve_temporal(
            &[original, successor],
            &[],
            &[edge],
            TemporalModeV1::Current,
        )
        .expect("resolution succeeds");

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].occurrence.anchor_id, anchor("successor"));
        assert_eq!(
            resolved.lineage_edges[0].kind,
            ResolutionLineageEdgeKind::Supersession
        );
    }

    #[test]
    fn forensic_retains_all_versions_and_lineage_without_suppression() {
        let original = occurrence(
            'a',
            "original",
            1,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(1),
            },
        );
        let correction = occurrence(
            'b',
            "correction",
            2,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(2),
            },
        );
        let edge = assertion(
            TemporalAssertionKindV1::Corrects,
            "correction",
            "original",
            2,
        );

        let resolved = resolve_temporal(
            &[original, correction],
            &[],
            &[edge],
            TemporalModeV1::Forensic,
        )
        .expect("resolution succeeds");

        assert_eq!(resolved.len(), 2);
        assert!(resolved.iter().all(|item| !item.conflicted));
        assert_eq!(resolved.lineage_edges.len(), 1);
    }

    #[test]
    fn resolver_filters_directly_constructed_unauthorized_assertions() {
        let original = occurrence(
            'a',
            "original",
            1,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(1),
            },
        );
        let correction = occurrence(
            'b',
            "correction",
            2,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(2),
            },
        );
        let mut edge = assertion(
            TemporalAssertionKindV1::Corrects,
            "correction",
            "original",
            2,
        );
        edge.evidence = ResolutionEvidence::new(
            SessionAuthorityClassV1::CanonicalObservation,
            ValidatedAuthorization::Unauthorized,
        );

        let resolved = resolve_temporal(
            &[original, correction],
            &[],
            &[edge],
            TemporalModeV1::Current,
        )
        .expect("resolution succeeds");

        assert_eq!(resolved.len(), 2);
        assert!(resolved.lineage_edges.is_empty());
        assert!(resolved.iter().all(|item| !item.conflicted));
    }
}

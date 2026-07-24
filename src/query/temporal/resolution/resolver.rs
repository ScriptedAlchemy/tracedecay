use std::collections::{BTreeMap, BTreeSet};

use tracedecay_domain::{
    LogicalCopyRecordV1, MessageOccurrenceIdV1, RetrievalAnchorId, SessionAuthorityClassV1,
    TemporalAssertionKindV1, TemporalModeV1, TemporalValidityV1,
};

use super::super::ports::{ExecutionControl, TemporalPortError};
use super::types::{
    ResolutionAssertion, ResolutionCheckpoint, ResolutionLineageEdge, ResolutionLineageEdgeKind,
    ResolutionOccurrence, ResolvedOccurrence, TemporalResolution,
};

fn checkpoint(
    control: &ExecutionControl,
    hook: &mut dyn FnMut(ResolutionCheckpoint) -> Result<(), TemporalPortError>,
    phase: ResolutionCheckpoint,
) -> Result<(), TemporalPortError> {
    control.checkpoint()?;
    hook(phase)
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

fn copy_sources(
    copies: &[LogicalCopyRecordV1],
    mode: TemporalModeV1,
    control: &ExecutionControl,
    hook: &mut dyn FnMut(ResolutionCheckpoint) -> Result<(), TemporalPortError>,
) -> Result<BTreeMap<MessageOccurrenceIdV1, MessageOccurrenceIdV1>, TemporalPortError> {
    let mut validated = Vec::with_capacity(copies.len());
    for copy in copies {
        checkpoint(control, hook, ResolutionCheckpoint::Copy)?;
        if copy.validate().is_ok()
            && copy
                .valid_time
                .is_representative_at(copy.knowledge_at, mode)
        {
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

pub fn copy_root(
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
    let copy_sources = copy_sources(copies, mode, control, hook)?;
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
                suppressed_anchors.remove(subject);
                suppressed_anchors.remove(object);
            }
        }
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

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

/// Reference cycle-membership oracle: an independent reachability DFS from every
/// node, O(V * (V + E)). Retained only as the equivalence baseline for
/// [`cycle_members_among`]; production uses the linear SCC pass below.
#[cfg(test)]
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

#[cfg(test)]
fn cycle_members_among_reference(
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

/// Iterative Tarjan work-stack frame: `node`, its in-subgraph successors, and
/// how many we have already descended into.
struct SccFrame {
    node: RetrievalAnchorId,
    children: Vec<RetrievalAnchorId>,
    next_child: usize,
}

/// Nodes that lie on a cycle within the subgraph induced by `nodes`.
///
/// A node reaches itself iff it belongs to a strongly connected component of
/// size greater than one, or it carries a self-edge — precisely the set the
/// former per-node reachability DFS ([`cycle_members_among_reference`])
/// computed, but in a single linear O(V + E) Tarjan pass instead of
/// O(V * (V + E)). Recursion is expressed with an explicit work stack so deep
/// chains cannot overflow the call stack.
fn cycle_members_among(
    nodes: &BTreeSet<RetrievalAnchorId>,
    descendants: &BTreeMap<RetrievalAnchorId, BTreeSet<RetrievalAnchorId>>,
    control: &ExecutionControl,
    hook: &mut dyn FnMut(ResolutionCheckpoint) -> Result<(), TemporalPortError>,
) -> Result<BTreeSet<RetrievalAnchorId>, TemporalPortError> {
    let children_in_subgraph = |node: &RetrievalAnchorId| -> Vec<RetrievalAnchorId> {
        descendants
            .get(node)
            .map(|set| {
                set.iter()
                    .filter(|child| nodes.contains(*child))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    };

    let mut index_of = BTreeMap::<RetrievalAnchorId, usize>::new();
    let mut lowlink = BTreeMap::<RetrievalAnchorId, usize>::new();
    let mut on_stack = BTreeSet::<RetrievalAnchorId>::new();
    let mut component_stack = Vec::<RetrievalAnchorId>::new();
    let mut next_index = 0_usize;
    let mut cyclic = BTreeSet::new();

    for root in nodes {
        checkpoint(control, hook, ResolutionCheckpoint::Evolution)?;
        if index_of.contains_key(root) {
            continue;
        }
        index_of.insert(root.clone(), next_index);
        lowlink.insert(root.clone(), next_index);
        next_index += 1;
        component_stack.push(root.clone());
        on_stack.insert(root.clone());
        let mut work = vec![SccFrame {
            node: root.clone(),
            children: children_in_subgraph(root),
            next_child: 0,
        }];

        while let Some(frame) = work.last_mut() {
            checkpoint(control, hook, ResolutionCheckpoint::Evolution)?;
            if frame.next_child < frame.children.len() {
                let child = frame.children[frame.next_child].clone();
                frame.next_child += 1;
                if let Some(&child_index) = index_of.get(&child) {
                    if on_stack.contains(&child) {
                        let node = frame.node.clone();
                        let low = lowlink[&node].min(child_index);
                        lowlink.insert(node, low);
                    }
                } else {
                    index_of.insert(child.clone(), next_index);
                    lowlink.insert(child.clone(), next_index);
                    next_index += 1;
                    component_stack.push(child.clone());
                    on_stack.insert(child.clone());
                    work.push(SccFrame {
                        node: child.clone(),
                        children: children_in_subgraph(&child),
                        next_child: 0,
                    });
                }
            } else {
                let node = frame.node.clone();
                let node_low = lowlink[&node];
                if node_low == index_of[&node] {
                    // SCC root: pop its members off the component stack.
                    let mut component = Vec::new();
                    while let Some(member) = component_stack.pop() {
                        on_stack.remove(&member);
                        let is_node = member == node;
                        component.push(member);
                        if is_node {
                            break;
                        }
                    }
                    let multi = component.len() > 1;
                    for member in component {
                        let self_loop = descendants
                            .get(&member)
                            .is_some_and(|set| set.contains(&member));
                        if multi || self_loop {
                            cyclic.insert(member);
                        }
                    }
                }
                work.pop();
                if let Some(parent) = work.last() {
                    let parent_node = parent.node.clone();
                    let low = lowlink[&parent_node].min(node_low);
                    lowlink.insert(parent_node, low);
                }
            }
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

/// Reference copy-root walk: an independent chain traversal per occurrence,
/// O(n^2) on a shared chain. Retained only as the equivalence baseline for
/// [`copy_root_memoized`], which production uses.
#[cfg(test)]
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

/// Memoized equivalent of [`copy_root`]. Resolving every occurrence's copy root
/// independently re-walks shared copy chains, which is O(n^2) on a long chain;
/// caching each acyclically-resolved root lets a shared chain be traversed once
/// overall (path compression).
///
/// Cyclic chains are deliberately left out of the cache: [`copy_root`] returns
/// the first occurrence revisited on that particular walk, which depends on the
/// start node, so folding it into the shared memo would corrupt other starts.
/// Those chains fall back to a full per-start walk, so the resolved root is
/// byte-for-byte identical to the reference implementation for every input.
fn copy_root_memoized(
    occurrence_id: &MessageOccurrenceIdV1,
    sources: &BTreeMap<MessageOccurrenceIdV1, MessageOccurrenceIdV1>,
    eligible_ids: &BTreeSet<MessageOccurrenceIdV1>,
    memo: &mut BTreeMap<MessageOccurrenceIdV1, MessageOccurrenceIdV1>,
    control: &ExecutionControl,
    hook: &mut dyn FnMut(ResolutionCheckpoint) -> Result<(), TemporalPortError>,
) -> Result<MessageOccurrenceIdV1, TemporalPortError> {
    let mut path = Vec::new();
    let mut on_path = BTreeSet::new();
    let mut current = occurrence_id.clone();
    loop {
        checkpoint(control, hook, ResolutionCheckpoint::Copy)?;
        if let Some(root) = memo.get(&current) {
            // A cached root is always the terminus of an acyclic chain, so every
            // occurrence walked to reach it shares that same root.
            let root = root.clone();
            for node in path {
                memo.insert(node, root.clone());
            }
            return Ok(root);
        }
        if !on_path.insert(current.clone()) {
            // Cycle: `current` is the first occurrence revisited on this walk,
            // exactly what `copy_root` returns. Start-dependent -> not cached.
            return Ok(current);
        }
        match sources.get(&current) {
            Some(parent) if eligible_ids.contains(parent) => {
                path.push(current.clone());
                current = parent.clone();
            }
            _ => break,
        }
    }
    // Natural terminus: `current` has no eligible parent and is the root of every
    // occurrence walked to reach it.
    for node in &path {
        memo.insert(node.clone(), current.clone());
    }
    memo.insert(current.clone(), current.clone());
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
    let mut copy_root_memo = BTreeMap::new();
    for occurrence in eligible {
        checkpoint(control, hook, ResolutionCheckpoint::Materialization)?;
        if suppressed_anchors.contains(&occurrence.anchor_id) {
            continue;
        }
        let representative_id = copy_root_memoized(
            &occurrence.occurrence_id,
            &copy_sources,
            &eligible_ids,
            &mut copy_root_memo,
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

#[cfg(test)]
mod algorithmic_equivalence_tests {
    //! Findings 9 and 10 equivalence: the memoized copy-root walk and the linear
    //! SCC cycle-membership pass must return byte-identical results to the
    //! quadratic reference implementations they replace, across randomized
    //! graphs that exercise chains, cycles, rho shapes, self-loops, branching,
    //! and disconnected components.
    use super::*;

    /// Deterministic xorshift64* PRNG so the sweep is reproducible.
    struct Rng(u64);

    impl Rng {
        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }

        fn below(&mut self, bound: usize) -> usize {
            (self.next_u64() % bound as u64) as usize
        }

        fn chance(&mut self, denominator: usize) -> bool {
            self.below(denominator) == 0
        }
    }

    fn oid(index: usize) -> MessageOccurrenceIdV1 {
        MessageOccurrenceIdV1::new(format!("sha256:{index:064x}")).expect("valid occurrence id")
    }

    fn aid(index: usize) -> RetrievalAnchorId {
        serde_json::from_str(&format!("\"anchor-{index}\"")).expect("valid anchor")
    }

    fn noop_hook() -> impl FnMut(ResolutionCheckpoint) -> Result<(), TemporalPortError> {
        |_checkpoint| Ok(())
    }

    #[test]
    fn memoized_copy_root_matches_reference() {
        let control = ExecutionControl::default();
        for seed in 1..=600_u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1));
            let node_count = 2 + rng.below(6);

            // Functional parent map: every occurrence has at most one source, as
            // produced by `copy_sources`. Parents may point anywhere (including
            // self), yielding chains, cycles, and rho shapes.
            let mut sources = BTreeMap::new();
            for index in 0..node_count {
                if rng.chance(4) {
                    continue; // root: no source
                }
                let parent = rng.below(node_count);
                sources.insert(oid(index), oid(parent));
            }
            let eligible_ids = (0..node_count)
                .filter(|_| !rng.chance(5))
                .map(oid)
                .collect::<BTreeSet<_>>();

            // Shared memo mirrors production: it accumulates across every start.
            let mut memo = BTreeMap::new();
            for index in 0..node_count {
                let start = oid(index);
                let mut reference_hook = noop_hook();
                let expected =
                    copy_root(&start, &sources, &eligible_ids, &control, &mut reference_hook)
                        .expect("reference copy root");
                let mut memo_hook = noop_hook();
                let actual = copy_root_memoized(
                    &start,
                    &sources,
                    &eligible_ids,
                    &mut memo,
                    &control,
                    &mut memo_hook,
                )
                .expect("memoized copy root");
                assert_eq!(
                    actual, expected,
                    "seed {seed} start {index}: sources={sources:?} eligible={eligible_ids:?}"
                );
            }
        }
    }

    #[test]
    fn scc_cycle_members_match_reference() {
        let control = ExecutionControl::default();
        for seed in 1..=800_u64 {
            let mut rng = Rng(seed.wrapping_mul(0xD1B5_4A32_D192_ED03).wrapping_add(7));
            let universe = 3 + rng.below(5);

            let mut descendants = BTreeMap::<RetrievalAnchorId, BTreeSet<RetrievalAnchorId>>::new();
            for parent in 0..universe {
                let mut children = BTreeSet::new();
                for child in 0..universe {
                    if rng.chance(3) {
                        children.insert(aid(child)); // self-edges allowed
                    }
                }
                if !children.is_empty() {
                    descendants.insert(aid(parent), children);
                }
            }
            // Induced subgraph: a random subset of the universe.
            let nodes = (0..universe)
                .filter(|_| !rng.chance(4))
                .map(aid)
                .collect::<BTreeSet<_>>();

            let mut reference_hook = noop_hook();
            let expected =
                cycle_members_among_reference(&nodes, &descendants, &control, &mut reference_hook)
                    .expect("reference cycle members");
            let mut scc_hook = noop_hook();
            let actual = cycle_members_among(&nodes, &descendants, &control, &mut scc_hook)
                .expect("scc cycle members");
            assert_eq!(
                actual, expected,
                "seed {seed}: nodes={nodes:?} descendants={descendants:?}"
            );
        }
    }
}

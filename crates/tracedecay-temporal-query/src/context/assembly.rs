use std::cmp::Ordering;
use std::collections::BTreeSet;

use tracedecay_domain::{
    CompactContextBundleV1, CompactContextConflictV1, CompactContextLineageEdgeV1,
    CompactContextOmissionV1, ContextOmissionReasonV1, RetrievalAnchorId, RetrievalGrainV1,
};

use super::super::hydration::HydrationBatch;
use super::super::ports::ExecutionControl;
use super::super::resolution::summary::{SummaryLineageRejection, SummaryOmission};
use super::admission::{
    choose_admission, materialize_admission, measure_context, prepare_admission, render_exact,
};
use super::wire::omission_reason;
use super::{
    CompactContext, ContextBudget, ContextError, ContextPayload, ContextUnavailable,
    MAX_CONTEXT_ANCHORS, MAX_CONTEXT_FRAME_ITEMS, MAX_CONTEXT_RECORDS, TemporalContextFrames,
    VersionedTokenEstimator,
};

pub fn assemble_context_with_frames_controlled(
    hydration: &HydrationBatch,
    grain: RetrievalGrainV1,
    frames: TemporalContextFrames,
    budget: ContextBudget,
    estimator: &impl VersionedTokenEstimator,
    control: &ExecutionControl,
) -> Result<CompactContext, ContextError> {
    if estimator.version() != budget.estimator_version {
        return Err(ContextError::EstimatorVersionMismatch);
    }
    assemble_context_parts_with_frames(
        &hydration.available,
        &hydration.unavailable,
        grain,
        frames,
        budget,
        estimator,
        control,
    )
}

#[cfg(test)]
pub fn assemble_context_parts<P: ContextPayload, U: ContextUnavailable>(
    available: &[P],
    unavailable: &[U],
    grain: RetrievalGrainV1,
    budget: ContextBudget,
    estimator: &impl VersionedTokenEstimator,
    control: &ExecutionControl,
) -> Result<CompactContext, ContextError> {
    assemble_context_parts_with_frames(
        available,
        unavailable,
        grain,
        TemporalContextFrames::default(),
        budget,
        estimator,
        control,
    )
}

pub fn assemble_context_parts_with_frames<P: ContextPayload, U: ContextUnavailable>(
    available: &[P],
    unavailable: &[U],
    grain: RetrievalGrainV1,
    mut frames: TemporalContextFrames,
    budget: ContextBudget,
    estimator: &impl VersionedTokenEstimator,
    control: &ExecutionControl,
) -> Result<CompactContext, ContextError> {
    validate_frozen_bounds(available, unavailable, &frames, budget.max_bytes)?;
    canonicalize_frames(&mut frames)?;
    // Build the sorted anchor-id index exactly once: both the privacy/overlap
    // validation and the later omission-clearing pass key off the same sorted
    // slice, so constructing and sorting it twice per call was pure waste.
    let mut available_ids = Vec::new();
    try_reserve(&mut available_ids, available.len())?;
    for payload in available {
        available_ids.push(payload.anchor_id().clone());
    }
    available_ids.sort();
    validate_privacy_and_anchor_overlap(&available_ids, unavailable, &frames)?;

    let summary_omissions = frames.summary_omissions;
    let mut bundle = CompactContextBundleV1 {
        omissions: frames.omissions,
        coverage: frames.coverage,
        conflicts: frames.conflicts,
        lineage: frames.lineage,
        ..CompactContextBundleV1::default()
    };
    let extra_omissions = unavailable
        .len()
        .checked_add(summary_omissions.len())
        .and_then(|count| count.checked_add(1))
        .ok_or(ContextError::BudgetExceeded {
            resource: "anchor count",
        })?;
    try_reserve(&mut bundle.omissions, extra_omissions)?;
    try_reserve(&mut bundle.continuation_anchors, available.len())?;
    try_reserve(&mut bundle.records, available.len())?;

    for unavailable in unavailable {
        control.checkpoint()?;
        bundle.omissions.push(CompactContextOmissionV1 {
            anchor_id: Some(unavailable.anchor_id().clone()),
            reason: omission_reason(unavailable.state()),
        });
    }
    preserve_rejected_summary_details(&mut bundle, &summary_omissions, control)?;
    for omission in &mut bundle.omissions {
        if !omission.reason.is_terminal_privacy()
            && omission
                .anchor_id
                .as_ref()
                .is_some_and(|anchor| available_ids.binary_search(anchor).is_ok())
        {
            omission.anchor_id = None;
        }
    }
    order_context_omissions(&mut bundle.omissions, unavailable);

    let policy = estimator.token_policy();
    let prepared = prepare_admission(
        available,
        grain,
        &bundle,
        &summary_omissions,
        &budget.estimator_version,
        policy,
        control,
    )?;
    let decision = choose_admission(
        &prepared,
        &bundle,
        &summary_omissions,
        &budget,
        policy,
        control,
    )?;
    materialize_admission(&mut bundle, available, grain, &prepared, decision, control)?;
    validate_bundle(&bundle)?;

    let measurement = measure_context(
        &bundle,
        &summary_omissions,
        &available[..decision.admitted],
        &budget.estimator_version,
        policy,
        control,
    )?;
    if measurement.bytes != decision.bytes || measurement.tokens() != decision.tokens {
        return Err(ContextError::InvalidBundle(
            "compact context admission accounting drifted".to_string(),
        ));
    }
    let rendered = render_exact(
        &bundle,
        &summary_omissions,
        &available[..decision.admitted],
        &budget.estimator_version,
        policy,
        measurement.bytes,
        control,
    )?;
    Ok(CompactContext {
        accounted_bytes: measurement.bytes,
        rendered,
        bundle,
        estimated_tokens: measurement.tokens(),
        estimator_version: budget.estimator_version,
    })
}

fn order_context_omissions<U: ContextUnavailable>(
    omissions: &mut [CompactContextOmissionV1],
    unavailable: &[U],
) {
    let hydration_position = |omission: &CompactContextOmissionV1| {
        omission.anchor_id.as_ref().and_then(|anchor_id| {
            unavailable
                .iter()
                .position(|item| item.anchor_id() == anchor_id)
        })
    };
    omissions.sort_by(
        |left, right| match (hydration_position(left), hydration_position(right)) {
            (Some(left), Some(right)) => left.cmp(&right),
            (Some(_), None) => Ordering::Greater,
            (None, Some(_)) => Ordering::Less,
            (None, None) => compare_omissions(left, right),
        },
    );
}

pub fn try_reserve<T>(values: &mut Vec<T>, additional: usize) -> Result<(), ContextError> {
    values
        .try_reserve(additional)
        .map_err(|_| ContextError::BudgetExceeded {
            resource: "allocation",
        })
}

fn validate_frozen_bounds<P: ContextPayload, U: ContextUnavailable>(
    available: &[P],
    unavailable: &[U],
    frames: &TemporalContextFrames,
    requested_max_bytes: u64,
) -> Result<(), ContextError> {
    for (count, limit, resource) in [
        (available.len(), MAX_CONTEXT_RECORDS, "record count"),
        (unavailable.len(), MAX_CONTEXT_ANCHORS, "anchor count"),
        (
            frames.omissions.len(),
            MAX_CONTEXT_FRAME_ITEMS,
            "omission count",
        ),
        (
            frames.conflicts.len(),
            MAX_CONTEXT_FRAME_ITEMS,
            "conflict count",
        ),
        (
            frames.lineage.len(),
            MAX_CONTEXT_FRAME_ITEMS,
            "lineage count",
        ),
        (
            frames.summary_omissions.len(),
            MAX_CONTEXT_FRAME_ITEMS,
            "summary omissions",
        ),
    ] {
        if count > limit {
            return Err(ContextError::BudgetExceeded { resource });
        }
    }
    let anchor_count = available
        .len()
        .checked_add(unavailable.len())
        .and_then(|count| count.checked_add(frames.omissions.len()))
        .and_then(|count| count.checked_add(frames.conflicts.len()))
        .and_then(|count| count.checked_add(frames.lineage.len().checked_mul(2)?))
        .and_then(|count| count.checked_add(frames.summary_omissions.len().checked_mul(2)?))
        .ok_or(ContextError::BudgetExceeded {
            resource: "anchor count",
        })?;
    if anchor_count > MAX_CONTEXT_ANCHORS {
        return Err(ContextError::BudgetExceeded {
            resource: "anchor count",
        });
    }
    if requested_max_bytes == 0 {
        return Err(ContextError::BudgetExceeded { resource: "byte" });
    }
    Ok(())
}

fn canonicalize_frames(frames: &mut TemporalContextFrames) -> Result<(), ContextError> {
    frames.omissions.sort_by(compare_omissions);
    frames.conflicts.sort_by(|left, right| {
        left.anchor_id
            .cmp(&right.anchor_id)
            .then_with(|| left.supporting_anchor_ids.cmp(&right.supporting_anchor_ids))
    });
    frames.lineage.sort_by(compare_lineage);
    if frames
        .lineage
        .windows(2)
        .any(|pair| compare_lineage(&pair[0], &pair[1]) == Ordering::Equal)
    {
        return Err(ContextError::InvalidBundle(
            "duplicate compact context lineage edge".to_string(),
        ));
    }
    frames.summary_omissions.sort_by(compare_summary_omissions);
    validate_lineage_cycles_are_conflicted(&frames.lineage, &frames.conflicts)
}

pub fn compare_omissions(
    left: &CompactContextOmissionV1,
    right: &CompactContextOmissionV1,
) -> Ordering {
    left.anchor_id
        .cmp(&right.anchor_id)
        .then_with(|| left.reason.cmp(&right.reason))
}

pub fn compare_lineage(
    left: &CompactContextLineageEdgeV1,
    right: &CompactContextLineageEdgeV1,
) -> Ordering {
    left.object_anchor_id
        .cmp(&right.object_anchor_id)
        .then_with(|| left.subject_anchor_id.cmp(&right.subject_anchor_id))
        .then_with(|| left.kind.cmp(&right.kind))
        .then_with(|| left.knowledge_at.cmp(&right.knowledge_at))
        .then_with(|| left.authority.cmp(&right.authority))
        .then_with(|| left.authorized.cmp(&right.authorized))
        .then_with(|| left.supporting_anchor_ids.cmp(&right.supporting_anchor_ids))
}

fn compare_summary_omissions(left: &SummaryOmission, right: &SummaryOmission) -> Ordering {
    left.summary_id
        .cmp(&right.summary_id)
        .then_with(|| left.anchor_id.cmp(&right.anchor_id))
        .then_with(|| compare_summary_rejections(&left.rejection, &right.rejection))
}

fn compare_summary_rejections(
    left: &SummaryLineageRejection,
    right: &SummaryLineageRejection,
) -> Ordering {
    summary_rejection_rank(left)
        .cmp(&summary_rejection_rank(right))
        .then_with(|| summary_rejection_value(left).cmp(summary_rejection_value(right)))
}

fn summary_rejection_rank(rejection: &SummaryLineageRejection) -> u8 {
    match rejection {
        SummaryLineageRejection::SessionMismatch => 0,
        SummaryLineageRejection::CreatedAfterCutoff => 1,
        SummaryLineageRejection::HorizonAfterCutoff => 2,
        SummaryLineageRejection::MissingValidHorizon => 3,
        SummaryLineageRejection::StaleSource { .. } => 4,
        SummaryLineageRejection::DeletedSource { .. } => 5,
        SummaryLineageRejection::RedactedSource { .. } => 6,
        SummaryLineageRejection::MissingSource { .. } => 7,
        SummaryLineageRejection::UnauthorizedSource { .. } => 8,
        SummaryLineageRejection::LockedSource { .. } => 9,
        SummaryLineageRejection::ExpiredSource { .. } => 10,
        SummaryLineageRejection::UnavailableSource { .. } => 11,
        SummaryLineageRejection::CycleSource { .. } => 12,
        SummaryLineageRejection::SourceBeyondKnowledgeHorizon { .. } => 13,
        SummaryLineageRejection::UnknownSourceValidTime { .. } => 14,
        SummaryLineageRejection::SourceBeyondValidHorizon { .. } => 15,
        SummaryLineageRejection::MissingPredecessor { .. } => 16,
        SummaryLineageRejection::IneligiblePredecessor { .. } => 17,
        SummaryLineageRejection::HorizonRegression { .. } => 18,
        SummaryLineageRejection::Cycle => 19,
    }
}

fn summary_rejection_value(rejection: &SummaryLineageRejection) -> &str {
    match rejection {
        SummaryLineageRejection::StaleSource { anchor_id }
        | SummaryLineageRejection::DeletedSource { anchor_id }
        | SummaryLineageRejection::RedactedSource { anchor_id }
        | SummaryLineageRejection::MissingSource { anchor_id }
        | SummaryLineageRejection::UnauthorizedSource { anchor_id }
        | SummaryLineageRejection::LockedSource { anchor_id }
        | SummaryLineageRejection::ExpiredSource { anchor_id }
        | SummaryLineageRejection::UnavailableSource { anchor_id }
        | SummaryLineageRejection::CycleSource { anchor_id }
        | SummaryLineageRejection::SourceBeyondKnowledgeHorizon { anchor_id }
        | SummaryLineageRejection::UnknownSourceValidTime { anchor_id }
        | SummaryLineageRejection::SourceBeyondValidHorizon { anchor_id } => anchor_id.as_str(),
        SummaryLineageRejection::MissingPredecessor {
            predecessor_summary_id,
        }
        | SummaryLineageRejection::IneligiblePredecessor {
            predecessor_summary_id,
        }
        | SummaryLineageRejection::HorizonRegression {
            predecessor_summary_id,
        } => predecessor_summary_id.as_str(),
        SummaryLineageRejection::SessionMismatch
        | SummaryLineageRejection::CreatedAfterCutoff
        | SummaryLineageRejection::HorizonAfterCutoff
        | SummaryLineageRejection::MissingValidHorizon
        | SummaryLineageRejection::Cycle => "",
    }
}

fn validate_privacy_and_anchor_overlap<U: ContextUnavailable>(
    available_ids: &[RetrievalAnchorId],
    unavailable: &[U],
    frames: &TemporalContextFrames,
) -> Result<(), ContextError> {
    if available_ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(ContextError::InvalidBundle(
            "duplicate available compact context anchor".to_string(),
        ));
    }
    for item in unavailable {
        if available_ids.binary_search(item.anchor_id()).is_ok() {
            return Err(ContextError::InvalidBundle(
                "compact context anchor is both available and unavailable".to_string(),
            ));
        }
    }
    for omission in &frames.omissions {
        if omission.reason.is_terminal_privacy()
            && omission
                .anchor_id
                .as_ref()
                .is_some_and(|anchor| available_ids.binary_search(anchor).is_ok())
        {
            return Err(ContextError::InvalidBundle(
                "available compact context anchor has a terminal omission".to_string(),
            ));
        }
    }
    for omission in &frames.summary_omissions {
        if terminal_rejected_detail(&omission.rejection)
            .is_some_and(|anchor| available_ids.binary_search(anchor).is_ok())
        {
            return Err(ContextError::InvalidBundle(
                "available compact context anchor is terminally rejected".to_string(),
            ));
        }
    }
    Ok(())
}

pub fn rejected_summary_detail_anchor(
    rejection: &SummaryLineageRejection,
) -> Option<&RetrievalAnchorId> {
    match rejection {
        SummaryLineageRejection::StaleSource { anchor_id }
        | SummaryLineageRejection::DeletedSource { anchor_id }
        | SummaryLineageRejection::RedactedSource { anchor_id }
        | SummaryLineageRejection::MissingSource { anchor_id }
        | SummaryLineageRejection::UnauthorizedSource { anchor_id }
        | SummaryLineageRejection::LockedSource { anchor_id }
        | SummaryLineageRejection::ExpiredSource { anchor_id }
        | SummaryLineageRejection::UnavailableSource { anchor_id }
        | SummaryLineageRejection::CycleSource { anchor_id }
        | SummaryLineageRejection::SourceBeyondKnowledgeHorizon { anchor_id }
        | SummaryLineageRejection::UnknownSourceValidTime { anchor_id }
        | SummaryLineageRejection::SourceBeyondValidHorizon { anchor_id } => Some(anchor_id),
        SummaryLineageRejection::SessionMismatch
        | SummaryLineageRejection::CreatedAfterCutoff
        | SummaryLineageRejection::HorizonAfterCutoff
        | SummaryLineageRejection::MissingValidHorizon
        | SummaryLineageRejection::MissingPredecessor { .. }
        | SummaryLineageRejection::IneligiblePredecessor { .. }
        | SummaryLineageRejection::HorizonRegression { .. }
        | SummaryLineageRejection::Cycle => None,
    }
}

fn terminal_rejected_detail(rejection: &SummaryLineageRejection) -> Option<&RetrievalAnchorId> {
    match rejection {
        SummaryLineageRejection::DeletedSource { anchor_id }
        | SummaryLineageRejection::RedactedSource { anchor_id }
        | SummaryLineageRejection::UnauthorizedSource { anchor_id }
        | SummaryLineageRejection::LockedSource { anchor_id }
        | SummaryLineageRejection::ExpiredSource { anchor_id } => Some(anchor_id),
        _ => None,
    }
}

fn terminal_omission_reason(reason: ContextOmissionReasonV1) -> bool {
    matches!(
        reason,
        ContextOmissionReasonV1::Unauthorized
            | ContextOmissionReasonV1::Redacted
            | ContextOmissionReasonV1::Deleted
            | ContextOmissionReasonV1::RetentionExpired
            | ContextOmissionReasonV1::Locked
    )
}

fn rejected_detail_omission_reason(rejection: &SummaryLineageRejection) -> ContextOmissionReasonV1 {
    match rejection {
        SummaryLineageRejection::UnauthorizedSource { .. }
        | SummaryLineageRejection::SessionMismatch => ContextOmissionReasonV1::Unauthorized,
        SummaryLineageRejection::DeletedSource { .. } => ContextOmissionReasonV1::Deleted,
        SummaryLineageRejection::RedactedSource { .. } => ContextOmissionReasonV1::Redacted,
        SummaryLineageRejection::ExpiredSource { .. } => ContextOmissionReasonV1::RetentionExpired,
        SummaryLineageRejection::LockedSource { .. } => ContextOmissionReasonV1::Locked,
        SummaryLineageRejection::UnavailableSource { .. } => ContextOmissionReasonV1::Unavailable,
        _ => ContextOmissionReasonV1::SummaryHorizonMismatch,
    }
}

fn preserve_rejected_summary_details(
    bundle: &mut CompactContextBundleV1,
    summary_omissions: &[SummaryOmission],
    control: &ExecutionControl,
) -> Result<(), ContextError> {
    let mut claimed = Vec::new();
    try_reserve(
        &mut claimed,
        bundle
            .omissions
            .len()
            .checked_add(summary_omissions.len())
            .ok_or(ContextError::BudgetExceeded {
                resource: "anchor count",
            })?,
    )?;
    for omission in &bundle.omissions {
        if let Some(anchor_id) = &omission.anchor_id {
            claimed.push(anchor_id.clone());
        }
    }
    claimed.sort();
    for omission in summary_omissions {
        control.checkpoint()?;
        let Some(detail) = rejected_summary_detail_anchor(&omission.rejection) else {
            continue;
        };
        match claimed.binary_search(detail) {
            Ok(_) => continue,
            Err(index) => claimed.insert(index, detail.clone()),
        }
        if bundle.omissions.len() >= MAX_CONTEXT_FRAME_ITEMS {
            return Err(ContextError::BudgetExceeded {
                resource: "omission count",
            });
        }
        bundle.omissions.push(CompactContextOmissionV1 {
            anchor_id: Some(detail.clone()),
            reason: rejected_detail_omission_reason(&omission.rejection),
        });
    }
    Ok(())
}

fn validate_lineage_cycles_are_conflicted(
    lineage: &[CompactContextLineageEdgeV1],
    conflicts: &[CompactContextConflictV1],
) -> Result<(), ContextError> {
    for edge in lineage {
        edge.validate()
            .map_err(|error| ContextError::InvalidBundle(error.to_string()))?;
    }
    let node_capacity = lineage
        .len()
        .checked_mul(2)
        .ok_or(ContextError::BudgetExceeded {
            resource: "lineage count",
        })?;
    let mut nodes = Vec::new();
    try_reserve(&mut nodes, node_capacity)?;
    for edge in lineage {
        nodes.push(edge.object_anchor_id.clone());
        nodes.push(edge.subject_anchor_id.clone());
    }
    nodes.sort();
    nodes.dedup();
    let mut out_counts = zeroed_usize_vec(nodes.len())?;
    let mut indegree = zeroed_usize_vec(nodes.len())?;
    for edge in lineage {
        let source = nodes
            .binary_search(&edge.object_anchor_id)
            .map_err(|_| ContextError::InvalidBundle("lineage source missing".to_string()))?;
        let target = nodes
            .binary_search(&edge.subject_anchor_id)
            .map_err(|_| ContextError::InvalidBundle("lineage target missing".to_string()))?;
        out_counts[source] =
            out_counts[source]
                .checked_add(1)
                .ok_or(ContextError::BudgetExceeded {
                    resource: "lineage count",
                })?;
        indegree[target] = indegree[target]
            .checked_add(1)
            .ok_or(ContextError::BudgetExceeded {
                resource: "lineage count",
            })?;
    }
    let mut offsets = zeroed_usize_vec(nodes.len().saturating_add(1))?;
    for index in 0..nodes.len() {
        offsets[index + 1] =
            offsets[index]
                .checked_add(out_counts[index])
                .ok_or(ContextError::BudgetExceeded {
                    resource: "lineage count",
                })?;
    }
    let mut cursors = offsets[..nodes.len()].to_vec();
    let mut targets = zeroed_usize_vec(lineage.len())?;
    for edge in lineage {
        let source = nodes
            .binary_search(&edge.object_anchor_id)
            .map_err(|_| ContextError::InvalidBundle("lineage source missing".to_string()))?;
        let target = nodes
            .binary_search(&edge.subject_anchor_id)
            .map_err(|_| ContextError::InvalidBundle("lineage target missing".to_string()))?;
        targets[cursors[source]] = target;
        cursors[source] += 1;
    }
    let mut queue = Vec::new();
    try_reserve(&mut queue, nodes.len())?;
    for (index, degree) in indegree.iter().enumerate() {
        if *degree == 0 {
            queue.push(index);
        }
    }
    let mut visited = 0_usize;
    let mut cursor = 0_usize;
    while cursor < queue.len() {
        let node = queue[cursor];
        cursor += 1;
        visited += 1;
        for target in &targets[offsets[node]..offsets[node + 1]] {
            indegree[*target] -= 1;
            if indegree[*target] == 0 {
                queue.push(*target);
            }
        }
    }
    if visited != nodes.len() {
        let conflicted = conflicts
            .iter()
            .map(|conflict| &conflict.anchor_id)
            .collect::<BTreeSet<_>>();
        for start in 0..nodes.len() {
            if indegree[start] == 0 {
                continue;
            }
            let mut stack = targets[offsets[start]..offsets[start + 1]].to_vec();
            let mut seen = vec![false; nodes.len()];
            seen[start] = true;
            let mut cyclic = false;
            while let Some(node) = stack.pop() {
                if node == start {
                    cyclic = true;
                    break;
                }
                if std::mem::replace(&mut seen[node], true) {
                    continue;
                }
                stack.extend_from_slice(&targets[offsets[node]..offsets[node + 1]]);
            }
            if cyclic && !conflicted.contains(&nodes[start]) {
                return Err(ContextError::InvalidBundle(
                    "unresolved compact context lineage cycle".to_string(),
                ));
            }
        }
    }
    Ok(())
}

fn zeroed_usize_vec(len: usize) -> Result<Vec<usize>, ContextError> {
    let mut values = Vec::new();
    try_reserve(&mut values, len)?;
    values.resize(len, 0);
    Ok(values)
}

trait TerminalPrivacyReason {
    fn is_terminal_privacy(&self) -> bool;
}

impl TerminalPrivacyReason for ContextOmissionReasonV1 {
    fn is_terminal_privacy(&self) -> bool {
        terminal_omission_reason(*self)
    }
}

pub fn validate_bundle(bundle: &CompactContextBundleV1) -> Result<(), ContextError> {
    bundle
        .validate()
        .map_err(|error| ContextError::InvalidBundle(error.to_string()))
}

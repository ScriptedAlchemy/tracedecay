use std::cmp::Ordering;

use tracedecay_domain::{CanonicalRelationEdgeV1, EdgeAuthorityV1};

use super::CodeGraphProjectionError;

#[derive(Clone)]
pub(super) struct FrontierPath {
    pub(super) segments: Vec<CanonicalRelationEdgeV1>,
    pub(super) weakest: Option<EdgeAuthorityV1>,
    pub(super) score: u64,
}

impl FrontierPath {
    pub(super) fn seed() -> Self {
        Self {
            segments: Vec::new(),
            weakest: None,
            score: u64::MAX,
        }
    }

    pub(super) fn extended(&self, segment: &CanonicalRelationEdgeV1) -> Self {
        let weakest = self.weakest.map_or(segment.authority, |current| {
            current.weakest(segment.authority)
        });
        let mut segments = self.segments.clone();
        segments.push(segment.clone());
        Self {
            score: graph_score_micros(segments.len(), weakest),
            segments,
            weakest: Some(weakest),
        }
    }
}

pub(super) fn admit_frontier_path(frontier: &mut Vec<FrontierPath>, candidate: FrontierPath) {
    if let Some(depth) = frontier.first().map(|path| path.segments.len()) {
        if depth < candidate.segments.len() {
            return;
        }
        if depth > candidate.segments.len() {
            frontier.clear();
        }
    }
    for current in frontier.iter() {
        if current.score >= candidate.score
            && !compare_paths(&current.segments, &candidate.segments).is_gt()
        {
            return;
        }
    }
    let mut retained = Vec::with_capacity(frontier.len() + 1);
    for current in frontier.drain(..) {
        if candidate.score < current.score
            || compare_paths(&candidate.segments, &current.segments).is_gt()
        {
            retained.push(current);
        }
    }
    retained.push(candidate);
    retained.sort_by(|left, right| compare_paths(&left.segments, &right.segments));
    *frontier = retained;
}

pub(super) fn best_frontier_path(
    paths: Vec<FrontierPath>,
) -> Result<FrontierPath, CodeGraphProjectionError> {
    let mut best = None::<FrontierPath>;
    for path in paths {
        let improves = best.as_ref().is_none_or(|current| {
            path.score > current.score
                || (path.score == current.score
                    && compare_paths(&path.segments, &current.segments).is_lt())
        });
        if improves {
            best = Some(path);
        }
    }
    best.ok_or_else(|| {
        CodeGraphProjectionError::Corrupt("code graph path frontier is empty".to_owned())
    })
}

pub(super) fn compare_paths(
    left: &[CanonicalRelationEdgeV1],
    right: &[CanonicalRelationEdgeV1],
) -> Ordering {
    left.iter()
        .map(|edge| {
            (
                &edge.from_occurrence,
                &edge.to_occurrence,
                edge.kind,
                edge.authority,
                edge.evidence_span,
            )
        })
        .cmp(right.iter().map(|edge| {
            (
                &edge.from_occurrence,
                &edge.to_occurrence,
                edge.kind,
                edge.authority,
                edge.evidence_span,
            )
        }))
}

fn graph_score_micros(path_len: usize, authority: EdgeAuthorityV1) -> u64 {
    let depth_bonus = 1_000_000u64.saturating_sub((path_len as u64).saturating_mul(50_000));
    let authority_bonus = match authority {
        EdgeAuthorityV1::SyntaxExact => 40_000,
        EdgeAuthorityV1::NameResolved => 30_000,
        EdgeAuthorityV1::CompilerOrLspResolved => 20_000,
        EdgeAuthorityV1::DynamicObserved => 10_000,
        EdgeAuthorityV1::HeuristicCandidate => 5_000,
        EdgeAuthorityV1::UnknownUnsupported => 1_000,
    };
    depth_bonus.saturating_add(authority_bonus)
}

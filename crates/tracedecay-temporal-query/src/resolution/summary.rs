use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use tracedecay_domain::{
    RetrievalAnchorId, SessionId, SessionSummaryIdV1, SessionSummaryRecordV1, TemporalModeV1,
    TemporalValidityV1, UtcMicros,
};

use super::super::ports::{ExecutionControl, TemporalPortError};

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

fn summary_rejection_order(rejection: &SummaryLineageRejection) -> (u8, u8, &str) {
    let (privacy, kind, identity) = match rejection {
        SummaryLineageRejection::UnauthorizedSource { anchor_id } => (0, 0, anchor_id.as_str()),
        SummaryLineageRejection::SessionMismatch => (0, 1, ""),
        SummaryLineageRejection::RedactedSource { anchor_id } => (1, 0, anchor_id.as_str()),
        SummaryLineageRejection::DeletedSource { anchor_id } => (1, 1, anchor_id.as_str()),
        SummaryLineageRejection::ExpiredSource { anchor_id } => (1, 2, anchor_id.as_str()),
        SummaryLineageRejection::LockedSource { anchor_id } => (2, 0, anchor_id.as_str()),
        SummaryLineageRejection::UnavailableSource { anchor_id } => (3, 0, anchor_id.as_str()),
        SummaryLineageRejection::CycleSource { anchor_id } => (3, 1, anchor_id.as_str()),
        SummaryLineageRejection::MissingSource { anchor_id } => (3, 2, anchor_id.as_str()),
        SummaryLineageRejection::StaleSource { anchor_id } => (4, 0, anchor_id.as_str()),
        SummaryLineageRejection::SourceBeyondKnowledgeHorizon { anchor_id } => {
            (5, 0, anchor_id.as_str())
        }
        SummaryLineageRejection::UnknownSourceValidTime { anchor_id } => (5, 1, anchor_id.as_str()),
        SummaryLineageRejection::SourceBeyondValidHorizon { anchor_id } => {
            (5, 2, anchor_id.as_str())
        }
        SummaryLineageRejection::CreatedAfterCutoff => (6, 0, ""),
        SummaryLineageRejection::HorizonAfterCutoff => (6, 1, ""),
        SummaryLineageRejection::MissingValidHorizon => (6, 2, ""),
        SummaryLineageRejection::MissingPredecessor {
            predecessor_summary_id,
        } => (7, 0, predecessor_summary_id.as_str()),
        SummaryLineageRejection::IneligiblePredecessor {
            predecessor_summary_id,
        } => (7, 1, predecessor_summary_id.as_str()),
        SummaryLineageRejection::HorizonRegression {
            predecessor_summary_id,
        } => (7, 2, predecessor_summary_id.as_str()),
        SummaryLineageRejection::Cycle => (7, 3, ""),
    };
    (privacy, kind, identity)
}

fn prefer_summary_rejection(
    current: &mut Option<SummaryLineageRejection>,
    candidate: SummaryLineageRejection,
) {
    let replace = current.as_ref().is_none_or(|existing| {
        summary_rejection_order(&candidate) < summary_rejection_order(existing)
    });
    if replace {
        *current = Some(candidate);
    }
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
    let mut rejection = None;
    if let TemporalModeV1::AsOf { cutoff } = mode
        && summary.created_at() > cutoff
    {
        prefer_summary_rejection(&mut rejection, SummaryLineageRejection::CreatedAfterCutoff);
    }
    let valid_through = horizon.valid_through;
    if matches!(mode, TemporalModeV1::AsOf { .. }) && valid_through.is_none() {
        prefer_summary_rejection(&mut rejection, SummaryLineageRejection::MissingValidHorizon);
    }
    if let (TemporalModeV1::AsOf { cutoff }, Some(valid_through)) = (mode, valid_through)
        && (horizon.knowledge_through > cutoff || valid_through > cutoff)
    {
        prefer_summary_rejection(&mut rejection, SummaryLineageRejection::HorizonAfterCutoff);
    }
    for anchor_id in summary.source_anchors() {
        control.checkpoint()?;
        let state = source_states
            .get(anchor_id)
            .copied()
            .unwrap_or(SummarySourceState::Missing);
        let candidate = match state {
            SummarySourceState::Covered {
                knowledge_at,
                valid_time,
            } => {
                if knowledge_at > horizon.knowledge_through {
                    Some(SummaryLineageRejection::SourceBeyondKnowledgeHorizon {
                        anchor_id: anchor_id.clone(),
                    })
                } else {
                    match (valid_time, valid_through) {
                        (TemporalValidityV1::Known { valid_at }, Some(valid_through))
                            if valid_at <= valid_through =>
                        {
                            None
                        }
                        (TemporalValidityV1::Known { .. }, Some(_)) => {
                            Some(SummaryLineageRejection::SourceBeyondValidHorizon {
                                anchor_id: anchor_id.clone(),
                            })
                        }
                        // Sources routinely carry no valid-time assertion (all
                        // ingested messages today): that uncertainty is already
                        // surfaced per-occurrence through the coverage
                        // `unknown` axis, so it must not reject the summary's
                        // whole lineage — only a provably out-of-horizon
                        // source does.
                        (TemporalValidityV1::Unknown, Some(_)) => None,
                        (_, None) => None,
                    }
                }
            }
            SummarySourceState::Stale => Some(SummaryLineageRejection::StaleSource {
                anchor_id: anchor_id.clone(),
            }),
            SummarySourceState::Deleted => Some(SummaryLineageRejection::DeletedSource {
                anchor_id: anchor_id.clone(),
            }),
            SummarySourceState::Redacted => Some(SummaryLineageRejection::RedactedSource {
                anchor_id: anchor_id.clone(),
            }),
            SummarySourceState::Missing => Some(SummaryLineageRejection::MissingSource {
                anchor_id: anchor_id.clone(),
            }),
            SummarySourceState::Unauthorized => Some(SummaryLineageRejection::UnauthorizedSource {
                anchor_id: anchor_id.clone(),
            }),
            SummarySourceState::Locked => Some(SummaryLineageRejection::LockedSource {
                anchor_id: anchor_id.clone(),
            }),
            SummarySourceState::Expired => Some(SummaryLineageRejection::ExpiredSource {
                anchor_id: anchor_id.clone(),
            }),
            SummarySourceState::Unavailable => Some(SummaryLineageRejection::UnavailableSource {
                anchor_id: anchor_id.clone(),
            }),
            SummarySourceState::Cycle => Some(SummaryLineageRejection::CycleSource {
                anchor_id: anchor_id.clone(),
            }),
        };
        if let Some(candidate) = candidate {
            prefer_summary_rejection(&mut rejection, candidate);
        }
    }
    Ok(rejection)
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

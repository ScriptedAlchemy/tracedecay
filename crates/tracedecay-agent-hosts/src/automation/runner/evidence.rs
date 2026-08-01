use serde::Serialize;
use serde_json::{Value, json};
use tracedecay_domain::TemporalCoverageCountsV1;

use crate::ports::session_evidence::{LcmGrepHit, LcmGrepSort, LcmScope};

use crate::analytics::{ToolUsageObservation, underused_tool_family_signals};
use crate::automation::artifacts::sha256_json;
use crate::automation::managed_skills::list_managed_skills;
use crate::automation::skill_usage::{
    DEFAULT_SKILL_OVERLAP_LIMIT, ingest_project_analytics_events, skill_overlap_candidates,
    stale_skill_recommendations, summarize_skill_usage,
};
use crate::automation::skill_writer::{
    skill_improvement_recommendations, support_file_evidence as skill_writer_support_file_evidence,
};
use crate::automation::text::truncate_chars_for_prompt;
use crate::errors::Result;
use crate::ports::session_store::AutomationSessionStore;
use crate::tracedecay::current_timestamp;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use super::retrieval::{
    AutomationSessionRetrieval, AutomationTemporalRetrieval, retrieve_automation_session_evidence,
};
use super::session_reflector::{
    SessionReflectorAutomationOptions, default_session_provider, default_session_reflection_query,
};
use super::skill_writer::{
    SkillWriterAutomationOptions, default_skill_writer_provider, default_skill_writer_query,
};
/// Bounds for the session-replay evidence channel. Worst case per session is
/// `(4 + 4) * 500 + 3 * 700 = 6_100` snippet chars, so the default three
/// sessions stay under ~5k tokens alongside the grep hits.
pub(super) const SESSION_REPLAY_HEAD_TURNS: usize = 4;

pub(super) const SESSION_REPLAY_TAIL_TURNS: usize = 4;

pub(super) const SESSION_REPLAY_SNIPPET_CHARS: usize = 500;

pub(super) const SESSION_REPLAY_SUMMARY_NODES: usize = 3;

pub(super) const SESSION_REPLAY_SUMMARY_CHARS: usize = 700;

const SKILL_ANALYTICS_IMPORT_LIMIT: usize = 2_000;

#[derive(Clone)]
#[doc(hidden)]
pub struct AutomationTemporalEvidenceItem {
    pub anchor_id: String,
    pub stable_id: String,
    pub provider: String,
    pub session_id: String,
    pub message_id: Option<String>,
    pub source_id: Option<String>,
    pub store_id: Option<i64>,
    pub role: Option<String>,
    pub ordinal: Option<i64>,
    pub session_total_messages: Option<u64>,
    pub knowledge_at_micros: i64,
    pub normalized_score_micros: u64,
    pub snippet: String,
}

#[doc(hidden)]
pub struct AutomationTemporalEvidence {
    pub items: Vec<AutomationTemporalEvidenceItem>,
    pub coverage: TemporalCoverageCountsV1,
}

pub(super) struct SerializedAutomationEvidence {
    pub(super) hits: Vec<CanonicalEvidenceHit>,
    pub(super) recent_session_slices: Option<Value>,
    pub(super) tool_usage: Vec<OwnedToolUsageObservation>,
    pub(super) coverage: TemporalCoverageCountsV1,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct CanonicalEvidenceHit {
    pub(super) kind: String,
    pub(super) provider: String,
    pub(super) session_id: String,
    pub(super) message_id: Option<String>,
    pub(super) node_id: Option<String>,
    pub(super) store_id: Option<i64>,
    pub(super) role: Option<String>,
    pub(super) snippet: String,
    pub(super) anchor_id: String,
    pub(super) stable_id: String,
    pub(super) knowledge_at_micros: i64,
    pub(super) normalized_score_micros: u64,
    pub(super) ordinal: Option<i64>,
}

impl CanonicalEvidenceHit {
    fn compatibility_hit(&self) -> LcmGrepHit {
        LcmGrepHit {
            kind: self.kind.clone(),
            provider: self.provider.clone(),
            session_id: self.session_id.clone(),
            message_id: self.message_id.clone(),
            node_id: self.node_id.clone(),
            store_id: self.store_id,
            role: self.role.clone(),
            snippet: self.snippet.clone(),
        }
    }
}

pub(super) struct OwnedToolUsageObservation {
    pub(super) tool_names: Option<String>,
    pub(super) metadata_json: Option<String>,
    pub(super) text: Option<String>,
}

#[derive(Clone, Copy)]
pub(super) struct AutomationEvidenceFilters<'a> {
    pub(super) provider: &'a str,
    pub(super) session_id: Option<&'a str>,
    pub(super) include_summaries: bool,
    pub(super) evidence_limit: usize,
    pub(super) include_recent_sessions: bool,
    pub(super) recent_sessions_limit: usize,
    pub(super) role: Option<&'a str>,
    pub(super) start_time: Option<i64>,
    pub(super) end_time: Option<i64>,
    pub(super) sort: LcmGrepSort,
}

pub(super) struct SkillWriterEvidenceBundle {
    pub(super) profile_root: PathBuf,
    pub(super) evidence: Value,
    pub(super) evidence_hash: Option<String>,
}

pub(super) enum SkillWriterEvidenceOutcome {
    Ready(SkillWriterEvidenceBundle),
    Skipped {
        reason: &'static str,
        evidence_hash: Option<String>,
    },
}

pub(super) struct SessionReflectorEvidenceBundle {
    pub(super) evidence: Value,
    pub(super) evidence_hash: Option<String>,
}

pub(super) enum SessionReflectorEvidenceOutcome {
    Ready(SessionReflectorEvidenceBundle),
    Skipped {
        reason: &'static str,
        evidence_hash: Option<String>,
    },
}

fn temporal_seconds(timestamp_micros: i64) -> i64 {
    if timestamp_micros.unsigned_abs() >= 100_000_000_000 {
        timestamp_micros / 1_000_000
    } else {
        timestamp_micros
    }
}

fn temporal_payload_text(payload: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(payload) else {
        return payload.to_string();
    };
    [
        "/payload/text",
        "/payload/content",
        "/payload/summary_text",
        "/text",
        "/content",
    ]
    .into_iter()
    .find_map(|pointer| value.pointer(pointer).and_then(Value::as_str))
    .unwrap_or(payload)
    .to_string()
}

fn tool_usage_observation(item: &AutomationTemporalEvidenceItem) -> OwnedToolUsageObservation {
    let value = serde_json::from_str::<Value>(&item.snippet).ok();
    OwnedToolUsageObservation {
        tool_names: value
            .as_ref()
            .and_then(|value| find_string_field(value, "tool_names")),
        metadata_json: value
            .as_ref()
            .and_then(|value| find_string_field(value, "metadata_json")),
        text: Some(value.as_ref().map_or_else(
            || item.snippet.clone(),
            |value| {
                find_string_field(value, "text")
                    .unwrap_or_else(|| temporal_payload_text(&item.snippet))
            },
        )),
    }
}

fn find_string_field(value: &Value, field: &str) -> Option<String> {
    match value {
        Value::Object(object) => object
            .get(field)
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                object
                    .values()
                    .find_map(|value| find_string_field(value, field))
            }),
        Value::Array(values) => values
            .iter()
            .find_map(|value| find_string_field(value, field)),
        _ => None,
    }
}

pub(super) fn find_i64_field_in_json(encoded: &str, field: &str) -> Option<i64> {
    fn visit(value: &Value, field: &str) -> Option<i64> {
        match value {
            Value::Object(object) => object
                .get(field)
                .and_then(Value::as_i64)
                .or_else(|| object.values().find_map(|value| visit(value, field))),
            Value::Array(values) => values.iter().find_map(|value| visit(value, field)),
            _ => None,
        }
    }

    serde_json::from_str(encoded)
        .ok()
        .and_then(|value| visit(&value, field))
}

pub(super) fn find_string_field_in_json(encoded: &str, field: &str) -> Option<String> {
    serde_json::from_str(encoded)
        .ok()
        .and_then(|value| find_string_field(&value, field))
}

pub(super) fn canonical_evidence_hash(value: &Value) -> String {
    fn canonicalize(value: &Value) -> Value {
        match value {
            Value::Array(values) => Value::Array(values.iter().map(canonicalize).collect()),
            Value::Object(object) => {
                let mut entries = object.iter().collect::<Vec<_>>();
                entries.sort_by(|left, right| left.0.cmp(right.0));
                let mut canonical = serde_json::Map::new();
                for (key, value) in entries {
                    canonical.insert(key.clone(), canonicalize(value));
                }
                Value::Object(canonical)
            }
            scalar => scalar.clone(),
        }
    }

    sha256_json(&canonicalize(value))
}

/// Names the evidence channels actually present so run artifacts can
/// distinguish replay-backed runs from grep-only runs.
fn evidence_mode_label(has_replay: bool) -> &'static str {
    if has_replay {
        "session_replay_with_grep"
    } else {
        "grep_only"
    }
}

fn session_reflector_replay_allowed(
    scope: LcmScope,
    session_id: Option<&str>,
    source: Option<&str>,
    role: Option<&str>,
    start_time: Option<i64>,
    end_time: Option<i64>,
) -> bool {
    if source.is_some() || role.is_some() || start_time.is_some() || end_time.is_some() {
        return false;
    }

    matches!(scope, LcmScope::All) || session_id.is_some()
}

fn normalized_non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn compare_evidence_items(
    left: &AutomationTemporalEvidenceItem,
    right: &AutomationTemporalEvidenceItem,
    sort: LcmGrepSort,
) -> std::cmp::Ordering {
    let primary = match sort {
        LcmGrepSort::Recency => right.knowledge_at_micros.cmp(&left.knowledge_at_micros),
        LcmGrepSort::Relevance => right
            .normalized_score_micros
            .cmp(&left.normalized_score_micros),
        LcmGrepSort::Hybrid => right
            .normalized_score_micros
            .cmp(&left.normalized_score_micros)
            .then_with(|| right.knowledge_at_micros.cmp(&left.knowledge_at_micros)),
    };
    primary
        .then_with(|| left.provider.cmp(&right.provider))
        .then_with(|| left.session_id.cmp(&right.session_id))
        .then_with(|| left.ordinal.cmp(&right.ordinal))
        .then_with(|| left.stable_id.cmp(&right.stable_id))
        .then_with(|| left.anchor_id.cmp(&right.anchor_id))
}

pub(super) fn validate_complete_evidence(
    evidence: &AutomationTemporalEvidence,
) -> std::result::Result<(), &'static str> {
    if evidence.coverage.hidden != 0
        || evidence.coverage.unknown != 0
        || evidence.coverage.redacted != 0
    {
        return Err("session_evidence_partial");
    }
    let anchors = evidence
        .items
        .iter()
        .map(|item| item.anchor_id.as_str())
        .collect::<BTreeSet<_>>();
    if anchors.len() != evidence.items.len()
        || u64::try_from(anchors.len()).ok() != Some(evidence.coverage.visible)
    {
        return Err("session_evidence_partial");
    }
    if evidence.items.iter().any(|item| {
        item.anchor_id.is_empty()
            || item.stable_id.is_empty()
            || item.provider.is_empty()
            || item.session_id.is_empty()
            || item.snippet.is_empty()
    }) {
        return Err("session_evidence_unavailable");
    }
    Ok(())
}

pub(super) fn serialize_automation_temporal_evidence(
    evidence: AutomationTemporalEvidence,
    filters: AutomationEvidenceFilters<'_>,
) -> SerializedAutomationEvidence {
    let mut filtered = evidence
        .items
        .into_iter()
        .filter(|item| {
            filters
                .session_id
                .is_none_or(|session_id| item.session_id == session_id)
                && filters
                    .role
                    .is_none_or(|role| item.role.as_deref() == Some(role))
                && filters
                    .start_time
                    .is_none_or(|start| temporal_seconds(item.knowledge_at_micros) >= start)
                && filters
                    .end_time
                    .is_none_or(|end| temporal_seconds(item.knowledge_at_micros) <= end)
                && (filters.include_summaries || item.message_id.is_some())
        })
        .collect::<Vec<_>>();
    filtered.sort_by(|left, right| compare_evidence_items(left, right, filters.sort));
    let hits = filtered
        .iter()
        .take(filters.evidence_limit)
        .map(|item| {
            let summary = item.message_id.is_none();
            CanonicalEvidenceHit {
                kind: if summary {
                    "summary_node".to_string()
                } else {
                    "raw_message".to_string()
                },
                provider: item.provider.clone(),
                session_id: item.session_id.clone(),
                message_id: item.message_id.clone(),
                node_id: summary.then(|| {
                    item.source_id
                        .clone()
                        .unwrap_or_else(|| item.stable_id.clone())
                }),
                store_id: item.store_id,
                role: item.role.clone(),
                snippet: truncate_chars_for_prompt(
                    &temporal_payload_text(&item.snippet),
                    if summary {
                        SESSION_REPLAY_SUMMARY_CHARS
                    } else {
                        SESSION_REPLAY_SNIPPET_CHARS
                    },
                ),
                anchor_id: item.anchor_id.clone(),
                stable_id: item.stable_id.clone(),
                knowledge_at_micros: item.knowledge_at_micros,
                normalized_score_micros: item.normalized_score_micros,
                ordinal: item.ordinal,
            }
        })
        .collect::<Vec<_>>();
    let mut selected_anchors = hits
        .iter()
        .map(|hit| hit.anchor_id.clone())
        .collect::<BTreeSet<_>>();
    let recent_session_slices = if filters.include_recent_sessions {
        recent_session_slices_from_temporal(
            &filtered,
            filters.session_id,
            filters.include_summaries,
            filters.recent_sessions_limit,
        )
        .map(|(slices, replay_anchors)| {
            selected_anchors.extend(replay_anchors);
            slices
        })
    } else {
        None
    };
    let tool_usage = filtered
        .iter()
        .filter(|item| selected_anchors.contains(&item.anchor_id))
        .map(tool_usage_observation)
        .collect::<Vec<_>>();
    SerializedAutomationEvidence {
        hits,
        recent_session_slices,
        tool_usage,
        coverage: TemporalCoverageCountsV1 {
            visible: selected_anchors.len() as u64,
            hidden: 0,
            unknown: 0,
            redacted: 0,
        },
    }
}

fn recent_session_slices_from_temporal(
    items: &[AutomationTemporalEvidenceItem],
    explicit_session_id: Option<&str>,
    include_summaries: bool,
    sessions_limit: usize,
) -> Option<(Value, BTreeSet<String>)> {
    let mut grouped: BTreeMap<(String, String), Vec<&AutomationTemporalEvidenceItem>> =
        BTreeMap::new();
    for item in items {
        grouped
            .entry((item.provider.clone(), item.session_id.clone()))
            .or_default()
            .push(item);
    }
    let mut session_order = grouped
        .iter()
        .map(|((provider, session_id), items)| {
            (
                items
                    .iter()
                    .map(|item| item.knowledge_at_micros)
                    .max()
                    .unwrap_or(i64::MIN),
                provider.clone(),
                session_id.clone(),
            )
        })
        .collect::<Vec<_>>();
    session_order.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.cmp(&right.2))
    });
    let sessions_limit = sessions_limit.clamp(1, 10);
    let mut selected_anchors = BTreeSet::new();
    let sessions = session_order
        .into_iter()
        .take(sessions_limit)
        .filter_map(|(_, provider, session_id)| {
            let mut session_items = grouped.remove(&(provider.clone(), session_id.clone()))?;
            session_items.sort_by(|left, right| {
                left.ordinal
                    .cmp(&right.ordinal)
                    .then_with(|| left.knowledge_at_micros.cmp(&right.knowledge_at_micros))
                    .then_with(|| left.anchor_id.cmp(&right.anchor_id))
            });
            let messages = session_items
                .iter()
                .filter(|item| item.message_id.is_some())
                .copied()
                .collect::<Vec<_>>();
            let ordinals = messages
                .iter()
                .map(|item| item.ordinal)
                .collect::<Option<Vec<_>>>()?;
            let total_messages = messages
                .iter().find_map(|item| item.session_total_messages)
                .or_else(|| {
                    let max = ordinals.iter().copied().max()?;
                    u64::try_from(max).ok()
                })?;
            let expected_ordinals = (1..=messages.len())
                .map(|ordinal| i64::try_from(ordinal).ok())
                .collect::<Option<Vec<_>>>()?;
            if ordinals != expected_ordinals
                || total_messages != u64::try_from(messages.len()).ok()?
                || messages
                    .iter()
                    .filter_map(|item| item.session_total_messages)
                    .any(|total| total != total_messages)
            {
                return None;
            }
            let head_count = messages.len().min(SESSION_REPLAY_HEAD_TURNS);
            let tail_start = messages
                .len()
                .saturating_sub(SESSION_REPLAY_TAIL_TURNS)
                .max(head_count);
            let replay_message = |item: &&AutomationTemporalEvidenceItem| {
                let text = temporal_payload_text(&item.snippet);
                let snippet = truncate_chars_for_prompt(&text, SESSION_REPLAY_SNIPPET_CHARS);
                json!({
                    "message_id": item.message_id,
                    "store_id": item.store_id,
                    "role": item.role,
                    "ordinal": item.ordinal,
                    "timestamp": temporal_seconds(item.knowledge_at_micros),
                    "snippet": snippet,
                    "truncated": snippet.chars().count() < text.chars().count(),
                    "provider": item.provider,
                    "anchor_id": item.anchor_id,
                    "stable_id": item.stable_id,
                    "knowledge_at_micros": item.knowledge_at_micros,
                })
            };
            let head = messages
                .iter()
                .take(head_count)
                .map(&replay_message)
                .collect::<Vec<_>>();
            let tail = messages
                .iter()
                .skip(tail_start)
                .map(replay_message)
                .collect::<Vec<_>>();
            for item in messages.iter().take(head_count) {
                selected_anchors.insert(item.anchor_id.clone());
            }
            for item in messages.iter().skip(tail_start) {
                selected_anchors.insert(item.anchor_id.clone());
            }
            let summary_nodes = if include_summaries {
                let nodes = session_items
                    .iter()
                    .filter(|item| item.message_id.is_none())
                    .take(SESSION_REPLAY_SUMMARY_NODES)
                    .map(|item| {
                        let text = temporal_payload_text(&item.snippet);
                        let snippet =
                            truncate_chars_for_prompt(&text, SESSION_REPLAY_SUMMARY_CHARS);
                        json!({
                            "node_id": item.source_id.clone().unwrap_or_else(|| item.stable_id.clone()),
                            "depth": 0,
                            "created_at": temporal_seconds(item.knowledge_at_micros),
                            "snippet": snippet,
                            "truncated": snippet.chars().count() < text.chars().count(),
                            "provider": item.provider,
                            "anchor_id": item.anchor_id,
                            "stable_id": item.stable_id,
                            "knowledge_at_micros": item.knowledge_at_micros,
                        })
                    })
                    .collect::<Vec<_>>();
                for item in session_items
                    .iter()
                    .filter(|item| item.message_id.is_none())
                    .take(SESSION_REPLAY_SUMMARY_NODES)
                {
                    selected_anchors.insert(item.anchor_id.clone());
                }
                nodes
            } else {
                Vec::new()
            };
            Some(json!({
                "provider": provider,
                "session_id": session_id,
                "total_messages": total_messages,
                "omitted_messages": total_messages.saturating_sub((head.len() + tail.len()) as u64),
                "head": head,
                "tail": tail,
                "summary_nodes": summary_nodes,
            }))
        })
        .collect::<Vec<_>>();
    if sessions.is_empty() {
        return None;
    }
    Some((
        json!({
            "mode": "recent_sessions",
            "session_selection": if explicit_session_id.is_some() {
                "explicit_session_id"
            } else {
                "recent_activity"
            },
            "sessions_limit": sessions_limit,
            "bounds": {
                "head_turns": SESSION_REPLAY_HEAD_TURNS,
                "tail_turns": SESSION_REPLAY_TAIL_TURNS,
                "snippet_chars": SESSION_REPLAY_SNIPPET_CHARS,
                "summary_nodes": if include_summaries {
                    SESSION_REPLAY_SUMMARY_NODES
                } else {
                    0
                },
                "summary_chars": SESSION_REPLAY_SUMMARY_CHARS,
            },
            "sessions": sessions,
        }),
        selected_anchors,
    ))
}

pub(super) async fn build_session_reflector_evidence(
    retrieval: &dyn AutomationSessionRetrieval,
    options: &SessionReflectorAutomationOptions,
) -> Result<SessionReflectorEvidenceOutcome> {
    let provider = normalized_non_empty(&options.provider).unwrap_or_else(default_session_provider);
    let query =
        normalized_non_empty(&options.query).unwrap_or_else(default_session_reflection_query);
    let evidence_limit = options.evidence_limit.clamp(1, 50);
    let session_id = options.session_id.as_deref().and_then(normalized_non_empty);
    let source = options.source.as_deref().and_then(normalized_non_empty);
    let role = options.role.as_deref().and_then(normalized_non_empty);

    if source.is_some()
        || role.is_some()
        || options.start_time.is_some()
        || options.end_time.is_some()
    {
        return Ok(SessionReflectorEvidenceOutcome::Skipped {
            reason: "session_evidence_filter_unavailable",
            evidence_hash: None,
        });
    }
    let include_recent_sessions = options.include_recent_sessions
        && session_reflector_replay_allowed(
            options.scope,
            session_id.as_deref(),
            source.as_deref(),
            role.as_deref(),
            options.start_time,
            options.end_time,
        );
    let filters = AutomationEvidenceFilters {
        provider: &provider,
        session_id: session_id.as_deref(),
        include_summaries: options.include_summaries,
        evidence_limit,
        include_recent_sessions,
        recent_sessions_limit: options.recent_sessions_limit,
        role: role.as_deref(),
        start_time: options.start_time,
        end_time: options.end_time,
        sort: options.sort,
    };
    let retrieval =
        retrieve_automation_session_evidence(retrieval, &query, options.scope, filters).await?;
    let serialized = match retrieval {
        AutomationTemporalRetrieval::Complete(evidence) => {
            match validate_complete_evidence(&evidence) {
                Ok(()) => serialize_automation_temporal_evidence(evidence, filters),
                Err(reason) => {
                    return Ok(SessionReflectorEvidenceOutcome::Skipped {
                        reason,
                        evidence_hash: None,
                    });
                }
            }
        }
        AutomationTemporalRetrieval::CompleteZero => serialize_automation_temporal_evidence(
            AutomationTemporalEvidence {
                items: Vec::new(),
                coverage: TemporalCoverageCountsV1::default(),
            },
            filters,
        ),
        AutomationTemporalRetrieval::Rejected(reason) => {
            return Ok(SessionReflectorEvidenceOutcome::Skipped {
                reason,
                evidence_hash: None,
            });
        }
    };
    let SerializedAutomationEvidence {
        hits,
        recent_session_slices,
        coverage,
        ..
    } = serialized;
    let evidence = json!({
        "evidence_mode": evidence_mode_label(recent_session_slices.is_some()),
        "temporal_mode": "forensic",
        "temporal_coverage": coverage,
        "provider": provider,
        "query": query,
        "scope": options.scope,
        "session_id": session_id,
        "include_summaries": options.include_summaries,
        "sort": options.sort,
        "source": source,
        "role": role,
        "start_time": options.start_time,
        "end_time": options.end_time,
        "recent_session_slices": recent_session_slices,
        "hits": hits,
    });
    let evidence_hash = Some(canonical_evidence_hash(&evidence));
    let has_grep_hits = evidence
        .get("hits")
        .and_then(Value::as_array)
        .is_some_and(|hits| !hits.is_empty());
    let has_replay_sessions = evidence
        .pointer("/recent_session_slices/sessions")
        .and_then(Value::as_array)
        .is_some_and(|sessions| !sessions.is_empty());
    if !has_grep_hits && !has_replay_sessions {
        return Ok(SessionReflectorEvidenceOutcome::Skipped {
            reason: "no_session_evidence",
            evidence_hash,
        });
    }

    Ok(SessionReflectorEvidenceOutcome::Ready(
        SessionReflectorEvidenceBundle {
            evidence,
            evidence_hash,
        },
    ))
}

pub(super) async fn build_skill_writer_evidence(
    retrieval: &dyn AutomationSessionRetrieval,
    analytics_project_root: Option<&std::path::Path>,
    analytics_db: Option<&dyn AutomationSessionStore>,
    options: SkillWriterAutomationOptions,
) -> Result<SkillWriterEvidenceOutcome> {
    let profile_root = match options.profile_root {
        Some(path) => path,
        None => crate::storage::default_profile_root()?,
    };
    let provider =
        normalized_non_empty(&options.provider).unwrap_or_else(default_skill_writer_provider);
    let query = normalized_non_empty(&options.query).unwrap_or_else(default_skill_writer_query);
    let evidence_limit = options.evidence_limit.clamp(1, 50);

    let filters = AutomationEvidenceFilters {
        provider: &provider,
        session_id: None,
        include_summaries: true,
        evidence_limit,
        include_recent_sessions: options.include_recent_sessions,
        recent_sessions_limit: options.recent_sessions_limit,
        role: None,
        start_time: None,
        end_time: None,
        sort: LcmGrepSort::Recency,
    };
    let retrieval =
        retrieve_automation_session_evidence(retrieval, &query, LcmScope::All, filters).await?;
    let serialized = match retrieval {
        AutomationTemporalRetrieval::Complete(evidence) => {
            match validate_complete_evidence(&evidence) {
                Ok(()) => serialize_automation_temporal_evidence(evidence, filters),
                Err(reason) => {
                    return Ok(SkillWriterEvidenceOutcome::Skipped {
                        reason,
                        evidence_hash: None,
                    });
                }
            }
        }
        AutomationTemporalRetrieval::CompleteZero => serialize_automation_temporal_evidence(
            AutomationTemporalEvidence {
                items: Vec::new(),
                coverage: TemporalCoverageCountsV1::default(),
            },
            filters,
        ),
        AutomationTemporalRetrieval::Rejected(reason) => {
            return Ok(SkillWriterEvidenceOutcome::Skipped {
                reason,
                evidence_hash: None,
            });
        }
    };
    let SerializedAutomationEvidence {
        hits,
        recent_session_slices,
        tool_usage,
        coverage,
    } = serialized;
    // Fail closed before any profile/skill-store I/O when temporal evidence is
    // empty: list_managed_skills creates the managed-skill root under
    // profile_root, which must not happen for denied/empty terminal skips.
    if hits.is_empty()
        && recent_session_slices
            .as_ref()
            .and_then(|slices| slices.pointer("/sessions").and_then(Value::as_array))
            .is_none_or(std::vec::Vec::is_empty)
    {
        return Ok(SkillWriterEvidenceOutcome::Skipped {
            reason: "no_skill_writer_evidence",
            evidence_hash: Some(canonical_evidence_hash(&json!({
                "evidence_mode": evidence_mode_label(recent_session_slices.is_some()),
                "temporal_mode": "forensic",
                "temporal_coverage": coverage,
                "provider": provider,
                "query": query,
                "recent_session_slices": recent_session_slices,
                "hits": hits,
            }))),
        });
    }
    let existing_skills = list_managed_skills(&profile_root).await?;
    if let (Some(project_root), Some(analytics_db)) = (analytics_project_root, analytics_db) {
        ingest_project_analytics_events(
            &profile_root,
            project_root,
            Some(analytics_db),
            SKILL_ANALYTICS_IMPORT_LIMIT,
        )
        .await?;
    }
    let skill_usage_summaries = summarize_skill_usage(&profile_root, &existing_skills).await?;
    let stale_recommendations = stale_skill_recommendations(
        &skill_usage_summaries,
        current_timestamp(),
        60 * 60 * 24 * 90,
    );
    let underused_tool_families =
        underused_tool_family_signals(tool_usage.iter().map(|row| ToolUsageObservation {
            tool_names: row.tool_names.as_deref(),
            metadata_json: row.metadata_json.as_deref(),
            text: row.text.as_deref(),
        }));
    let overlap_candidates =
        skill_overlap_candidates(&existing_skills, DEFAULT_SKILL_OVERLAP_LIMIT);
    let compatibility_hits = hits
        .iter()
        .map(CanonicalEvidenceHit::compatibility_hit)
        .collect::<Vec<_>>();
    let skill_improvement_recommendations = skill_improvement_recommendations(
        &compatibility_hits,
        &skill_usage_summaries,
        &stale_recommendations,
        &underused_tool_families,
        &overlap_candidates,
    );
    let evidence = json!({
        "evidence_mode": evidence_mode_label(recent_session_slices.is_some()),
        "temporal_mode": "forensic",
        "temporal_coverage": coverage,
        "provider": provider,
        "query": query,
        "recent_session_slices": recent_session_slices,
        "hits": hits,
        "skill_usage_summaries": skill_usage_summaries,
        "stale_recommendations": stale_recommendations,
        "underused_tool_families": underused_tool_families,
        "skill_overlap_candidates": overlap_candidates,
        "skill_improvement_recommendations": skill_improvement_recommendations,
        "existing_managed_skills": existing_skills
            .iter()
            .map(|skill| json!({
                "id": skill.metadata.id,
                "title": skill.metadata.title,
                "summary": skill.metadata.summary,
                "category": skill.metadata.category,
                "state": skill.metadata.state,
                "pinned": skill.metadata.pinned,
                "checksum": skill.metadata.checksum,
                "updated_at": skill.metadata.updated_at,
                "body_markdown": truncate_chars_for_prompt(&skill.body_markdown, 4000),
                "support_files": skill.support_files
                    .iter()
                    .map(skill_writer_support_file_evidence)
                    .collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>(),
    });
    let evidence_hash = Some(canonical_evidence_hash(&evidence));
    let has_grep_hits = evidence
        .get("hits")
        .and_then(Value::as_array)
        .is_some_and(|hits| !hits.is_empty());
    let has_replay_sessions = evidence
        .pointer("/recent_session_slices/sessions")
        .and_then(Value::as_array)
        .is_some_and(|sessions| !sessions.is_empty());
    if !has_grep_hits && !has_replay_sessions {
        return Ok(SkillWriterEvidenceOutcome::Skipped {
            reason: "no_skill_writer_evidence",
            evidence_hash,
        });
    }

    Ok(SkillWriterEvidenceOutcome::Ready(
        SkillWriterEvidenceBundle {
            profile_root,
            evidence,
            evidence_hash,
        },
    ))
}

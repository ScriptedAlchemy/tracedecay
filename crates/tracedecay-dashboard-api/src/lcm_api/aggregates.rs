use std::collections::{BTreeMap, BTreeSet};

use tracedecay_runtime_core::timeutil::format_yyyy_mm_dd;

use super::super::token_count::count_text_tokens;
use super::{
    DashboardLcmCanonicalMatchesV1, DashboardLcmCanonicalMessageV1, DashboardLcmCanonicalPageV1,
    DashboardLcmCanonicalSummaryV1, DashboardLcmReadRequestV1, DashboardLcmTimelineBucketV1,
    LcmTokenCountProvenanceV1,
};

pub(super) fn render_canonical_payload<T>(
    request: DashboardLcmReadRequestV1,
    page: DashboardLcmCanonicalPageV1,
    storage_scope: &str,
) -> Result<T, ()>
where
    T: serde::de::DeserializeOwned,
{
    let value = match request {
        DashboardLcmReadRequestV1::Overview { query, limit } => {
            overview_json(page, query, limit, storage_scope)?
        }
        DashboardLcmReadRequestV1::Search {
            query,
            limit,
            cursor: _,
            role,
            source,
            session_id,
            since,
            until,
        } => {
            let messages = page
                .messages
                .into_iter()
                .map(message_json)
                .collect::<Vec<_>>();
            let summary_nodes = page
                .summary_nodes
                .into_iter()
                .map(summary_json)
                .collect::<Vec<_>>();
            serde_json::json!({
                "path": "daemon://session-temporal",
                "storage_scope": storage_scope,
                "exists": true,
                "query": query,
                "limit": limit,
                "next_cursor": page.next_cursor,
                "engine": "canonical_temporal",
                "engine_detail": {
                    "messages": "canonical_hydration",
                    "summary_nodes": "canonical_temporal_relations"
                },
                "total": {
                    "messages": messages.len(),
                    "summary_nodes": summary_nodes.len()
                },
                "filters": {
                    "role": role,
                    "source": source,
                    "session_id": session_id,
                    "since": since,
                    "until": until
                },
                "matches": {"messages": messages, "summary_nodes": summary_nodes},
            })
        }
        DashboardLcmReadRequestV1::Session {
            session_id,
            limit,
            cursor: _,
        } => {
            let messages = page
                .messages
                .into_iter()
                .map(message_json)
                .collect::<Vec<_>>();
            let summary_nodes = page
                .summary_nodes
                .into_iter()
                .map(summary_json)
                .collect::<Vec<_>>();
            let returned_summary_nodes = saturating_usize_to_i64(summary_nodes.len());
            serde_json::json!({
                "path": "daemon://session-temporal",
                "storage_scope": storage_scope,
                "exists": page.stats.message_count > 0 || page.stats.summary_node_count > 0,
                "session_id": session_id,
                "limit": limit,
                "counts": {
                    "message_count": page.stats.message_count,
                    "summary_node_count": page.stats.summary_node_count,
                    "summary_token_count": page.stats.summary_token_count,
                    "source_token_count": page.stats.source_token_count
                },
                "messages": messages,
                "summary_nodes": summary_nodes,
                "has_more": page.has_more,
                "has_more_messages": page.has_more,
                "has_more_summary_nodes": page.stats.summary_node_count > returned_summary_nodes,
                "next_cursor": page.next_cursor
            })
        }
        DashboardLcmReadRequestV1::Timeline {
            bucket,
            session_id,
            limit,
        } => timeline_json(page, bucket, session_id, limit, storage_scope),
    };
    serde_json::from_value(value).map_err(|_| ())
}

fn overview_json(
    page: DashboardLcmCanonicalPageV1,
    query: String,
    limit: i64,
    storage_scope: &str,
) -> Result<serde_json::Value, ()> {
    let mut role_counts = BTreeMap::<String, i64>::new();
    let mut source_counts = BTreeMap::<String, i64>::new();
    let mut sessions = BTreeMap::<String, (i64, Option<i64>)>::new();
    for message in &page.messages {
        *role_counts.entry(message.role.clone()).or_default() += 1;
        *source_counts.entry(message.provider.clone()).or_default() += 1;
        let session = sessions.entry(message.session_id.clone()).or_default();
        session.0 = session.0.saturating_add(1);
        session.1 = max_optional_timestamp(session.1, message.timestamp);
    }
    let mut depth_counts = BTreeMap::<i64, i64>::new();
    let mut summary_sessions = BTreeSet::new();
    let mut source_token_count = Some(0_i64);
    let mut summary_token_count = Some(0_i64);
    let mut max_summary_depth = 0_i64;
    for summary in &page.summary_nodes {
        *depth_counts.entry(summary.depth).or_default() += 1;
        summary_sessions.insert(summary.session_id.clone());
        source_token_count = source_token_count
            .zip(summary.source_token_count)
            .map(|(total, tokens)| total.saturating_add(tokens));
        summary_token_count = summary_token_count
            .zip(summary.token_count)
            .map(|(total, tokens)| total.saturating_add(tokens));
        max_summary_depth = max_summary_depth.max(summary.depth);
    }

    let sessions_total = sessions.len();
    let mut latest_sessions = sessions
        .into_iter()
        .map(|(session_id, (message_count, last_timestamp))| {
            (session_id, message_count, last_timestamp)
        })
        .collect::<Vec<_>>();
    latest_sessions.sort_by(|left, right| right.2.cmp(&left.2).then_with(|| left.0.cmp(&right.0)));
    latest_sessions.truncate(i64_to_usize(limit));

    let mut latest_summary_nodes = page.summary_nodes.clone();
    latest_summary_nodes.sort_by(|left, right| {
        right
            .latest_at
            .unwrap_or(right.created_at)
            .cmp(&left.latest_at.unwrap_or(left.created_at))
            .then_with(|| right.created_at.cmp(&left.created_at))
            .then_with(|| left.node_id.cmp(&right.node_id))
    });
    latest_summary_nodes.truncate(i64_to_usize(limit));

    let matches = page
        .overview_matches
        .unwrap_or(DashboardLcmCanonicalMatchesV1 {
            messages: Vec::new(),
            summary_nodes: Vec::new(),
        });
    let mut role_counts = role_counts.into_iter().collect::<Vec<_>>();
    role_counts.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let mut source_counts = source_counts.into_iter().collect::<Vec<_>>();
    source_counts.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let messages_total = saturating_usize_to_i64(page.messages.len());
    let summary_nodes_total = saturating_usize_to_i64(page.summary_nodes.len());
    Ok(serde_json::json!({
        "path": "daemon://session-temporal",
        "storage_scope": storage_scope,
        "exists": true,
        "overview": {
            "messages_total": messages_total,
            "sessions_total": saturating_usize_to_i64(sessions_total),
            "summary_nodes_total": summary_nodes_total,
            "summary_node_sessions_total": saturating_usize_to_i64(summary_sessions.len()),
            "max_summary_depth": max_summary_depth,
            "role_counts": role_counts.into_iter().map(|(role, count)| serde_json::json!({
                "role": role,
                "count": count
            })).collect::<Vec<_>>(),
            "source_counts": source_counts.into_iter().map(|(source, count)| serde_json::json!({
                "source": source,
                "count": count
            })).collect::<Vec<_>>(),
            "depth_counts": depth_counts.into_iter().map(|(depth, count)| serde_json::json!({
                "depth": depth,
                "count": count
            })).collect::<Vec<_>>(),
            "compression": {
                "source_token_count": source_token_count,
                "token_count": summary_token_count,
                "ratio": compression_ratio(source_token_count, summary_token_count),
                "node_count": summary_nodes_total
            }
        },
        "latest_sessions": latest_sessions.into_iter().map(
            |(session_id, message_count, last_timestamp)| serde_json::json!({
                "session_id": session_id,
                "message_count": message_count,
                "last_store_id": null,
                "last_timestamp": last_timestamp
            })
        ).collect::<Vec<_>>(),
        "latest_summary_nodes": latest_summary_nodes.into_iter().map(summary_json).collect::<Vec<_>>(),
        "matches": {
            "messages": matches.messages.into_iter().take(i64_to_usize(limit)).map(message_json).collect::<Vec<_>>(),
            "summary_nodes": matches.summary_nodes.into_iter().take(i64_to_usize(limit)).map(summary_json).collect::<Vec<_>>()
        },
        "query": query,
        "limit": limit
    }))
}

#[derive(Clone, Copy, Debug)]
struct DisplayedContentTokenCount {
    token_count: Option<i64>,
    provenance: Option<LcmTokenCountProvenanceV1>,
}

fn displayed_content_token_count(
    message: &DashboardLcmCanonicalMessageV1,
) -> DisplayedContentTokenCount {
    match count_text_tokens(&message.content, "") {
        Some(token_count) => DisplayedContentTokenCount {
            token_count: Some(token_count),
            provenance: Some(LcmTokenCountProvenanceV1::O200kApproximate),
        },
        None => DisplayedContentTokenCount {
            token_count: None,
            provenance: None,
        },
    }
}

#[derive(Default)]
struct TokenCountAggregate {
    known_message_count: i64,
    unknown_message_count: i64,
    known_token_count: i64,
}

impl TokenCountAggregate {
    fn add(&mut self, count: DisplayedContentTokenCount) {
        match (count.token_count, count.provenance) {
            (Some(token_count), Some(LcmTokenCountProvenanceV1::O200kApproximate)) => {
                self.known_message_count = self.known_message_count.saturating_add(1);
                self.known_token_count = self.known_token_count.saturating_add(token_count);
            }
            _ => {
                self.unknown_message_count = self.unknown_message_count.saturating_add(1);
            }
        }
    }

    fn message_count(&self) -> i64 {
        self.known_message_count
            .saturating_add(self.unknown_message_count)
    }

    fn complete_token_count(&self) -> Option<i64> {
        (self.known_message_count > 0 && self.unknown_message_count == 0)
            .then_some(self.known_token_count)
    }

    fn provenance(&self) -> LcmTokenCountProvenanceV1 {
        if self.unknown_message_count > 0 || self.known_message_count == 0 {
            LcmTokenCountProvenanceV1::Unavailable
        } else {
            LcmTokenCountProvenanceV1::O200kApproximate
        }
    }
}

fn timeline_json(
    page: DashboardLcmCanonicalPageV1,
    bucket: DashboardLcmTimelineBucketV1,
    session_id: Option<String>,
    limit: i64,
    storage_scope: &str,
) -> serde_json::Value {
    let mut dated = BTreeMap::<String, TokenCountAggregate>::new();
    let mut undated = TokenCountAggregate::default();
    for message in &page.messages {
        let token_count = displayed_content_token_count(message);
        if let Some(timestamp) = message.timestamp {
            let key = utc_bucket(timestamp, bucket);
            dated.entry(key).or_default().add(token_count);
        } else {
            undated.add(token_count);
        }
    }
    let total_dated_buckets = saturating_usize_to_i64(dated.len());
    let keep = i64_to_usize(limit);
    let skip = dated.len().saturating_sub(keep);
    let buckets = dated
        .into_iter()
        .skip(skip)
        .map(|(bucket, aggregate)| {
            serde_json::json!({
                "bucket": bucket,
                "count": aggregate.message_count(),
                "token_count": aggregate.complete_token_count(),
                "token_count_provenance": aggregate.provenance(),
                "known_message_count": aggregate.known_message_count,
                "unknown_message_count": aggregate.unknown_message_count
            })
        })
        .collect::<Vec<_>>();
    let returned_buckets = saturating_usize_to_i64(buckets.len());
    let truncated = returned_buckets < total_dated_buckets;
    let next_before_bucket = truncated
        .then(|| {
            buckets
                .first()
                .and_then(|value| value["bucket"].as_str())
                .map(str::to_owned)
        })
        .flatten();

    let mut node_buckets = BTreeMap::<Option<String>, i64>::new();
    for summary in &page.summary_nodes {
        let timestamp = summary.latest_at.unwrap_or(summary.created_at);
        let key = utc_bucket(timestamp, bucket);
        *node_buckets.entry(Some(key)).or_default() += 1;
    }
    let node_skip = node_buckets.len().saturating_sub(keep);
    let node_buckets = node_buckets
        .into_iter()
        .skip(node_skip)
        .map(|(bucket, count)| serde_json::json!({"bucket": bucket, "count": count}))
        .collect::<Vec<_>>();
    serde_json::json!({
        "path": "daemon://session-temporal",
        "storage_scope": storage_scope,
        "exists": true,
        "bucket": bucket.as_str(),
        "session_id": session_id,
        "buckets": buckets,
        "node_buckets": node_buckets,
        "undated": {
            "count": undated.message_count(),
            "token_count": undated.complete_token_count(),
            "token_count_provenance": undated.provenance(),
            "known_message_count": undated.known_message_count,
            "unknown_message_count": undated.unknown_message_count
        },
        "coverage": {
            "limit": limit,
            "returned_buckets": returned_buckets,
            "total_dated_buckets": total_dated_buckets,
            "truncated": truncated,
            "ordering": "most_recent",
            "next_before_bucket": next_before_bucket
        }
    })
}

pub(super) fn is_aggregate_request(request: &DashboardLcmReadRequestV1) -> bool {
    matches!(
        request,
        DashboardLcmReadRequestV1::Overview { .. } | DashboardLcmReadRequestV1::Timeline { .. }
    )
}

pub(super) fn timeline_view_coverage(
    request: &DashboardLcmReadRequestV1,
    page: &DashboardLcmCanonicalPageV1,
) -> Option<(u64, u64, bool)> {
    let DashboardLcmReadRequestV1::Timeline { bucket, limit, .. } = request else {
        return None;
    };
    let buckets = page
        .messages
        .iter()
        .filter_map(|message| message.timestamp)
        .map(|timestamp| utc_bucket(timestamp, *bucket))
        .collect::<BTreeSet<_>>();
    let eligible = saturating_usize_to_u64(buckets.len());
    let requested = u64::try_from((*limit).max(0)).unwrap_or(u64::MAX);
    let examined = eligible.min(requested);
    Some((eligible, examined, eligible > examined))
}

pub(super) fn returned_count(page: &DashboardLcmCanonicalPageV1) -> u64 {
    let matches = page.overview_matches.as_ref().map_or(0, |matches| {
        matches
            .messages
            .len()
            .saturating_add(matches.summary_nodes.len())
    });
    u64::try_from(
        page.messages
            .len()
            .saturating_add(page.summary_nodes.len())
            .saturating_add(matches),
    )
    .unwrap_or(u64::MAX)
}

fn max_optional_timestamp(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (value @ Some(_), None) | (None, value @ Some(_)) => value,
        (None, None) => None,
    }
}

fn compression_ratio(source_tokens: Option<i64>, summary_tokens: Option<i64>) -> Option<f64> {
    let (source_tokens, summary_tokens) = source_tokens.zip(summary_tokens)?;
    (summary_tokens > 0)
        .then(|| (source_tokens as f64 / summary_tokens as f64 * 100.0).round() / 100.0)
}

fn utc_bucket(timestamp: i64, bucket: DashboardLcmTimelineBucketV1) -> String {
    let seconds_per_day = 86_400_i64;
    let days = timestamp.div_euclid(seconds_per_day);
    let seconds = timestamp.rem_euclid(seconds_per_day);
    let day = format_yyyy_mm_dd(days);
    match bucket {
        DashboardLcmTimelineBucketV1::Day => day,
        DashboardLcmTimelineBucketV1::Hour => {
            let hour = seconds / 3_600;
            format!("{day}T{hour:02}:00")
        }
    }
}

fn i64_to_usize(value: i64) -> usize {
    match usize::try_from(value.max(0)) {
        Ok(value) => value,
        Err(_) => usize::MAX,
    }
}

fn message_json(message: DashboardLcmCanonicalMessageV1) -> serde_json::Value {
    let token_count = displayed_content_token_count(&message);
    serde_json::json!({
        "store_id": null,
        "session_id": message.session_id,
        "role": message.role,
        "source": message.provider,
        "timestamp": message.timestamp,
        "token_count": token_count.token_count,
        "token_count_provenance": token_count.provenance,
        "content": message.content,
        "message_id": message.message_id,
        "ordinal": message.ordinal,
        "storage_kind": "canonical_temporal",
        "metadata_json": message.metadata_json,
        "tool_name": message.tool_names,
        "pinned": null,
        "summary_node_ids": [],
        "snippet": null
    })
}

fn summary_json(summary: DashboardLcmCanonicalSummaryV1) -> serde_json::Value {
    serde_json::json!({
        "node_id": summary.node_id,
        "session_id": summary.session_id,
        "depth": summary.depth,
        "category": "summary",
        "source_type": "canonical_temporal",
        "token_count": summary.token_count,
        "source_token_count": summary.source_token_count,
        "latest_at": summary.latest_at,
        "created_at": summary.created_at,
        "expand_hint": summary.expand_hint,
        "summary": summary.summary,
        "recency": summary.latest_at,
        "snippet": null
    })
}

fn saturating_usize_to_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn saturating_usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}
#[cfg(test)]
mod tests {
    use super::super::{
        DashboardLcmCanonicalMessageV1, DashboardLcmCanonicalPageV1, DashboardLcmCanonicalStatsV1,
        DashboardLcmCanonicalSummaryV1, DashboardLcmTimelineBucketV1, parse_optional_i64,
        trimmed_nonempty,
    };
    use super::{message_json, overview_json, timeline_json};

    fn aggregate_page() -> DashboardLcmCanonicalPageV1 {
        DashboardLcmCanonicalPageV1 {
            messages: vec![
                DashboardLcmCanonicalMessageV1 {
                    session_id: "session.older".to_owned(),
                    provider: "codex".to_owned(),
                    role: "user".to_owned(),
                    timestamp: Some(0),
                    ordinal: 1,
                    content: "four".to_owned(),
                    message_id: "message.older".to_owned(),
                    metadata_json: None,
                    tool_names: None,
                },
                DashboardLcmCanonicalMessageV1 {
                    session_id: "session.newer".to_owned(),
                    provider: "claude".to_owned(),
                    role: "assistant".to_owned(),
                    timestamp: Some(86_400),
                    ordinal: 1,
                    content: "eight888".to_owned(),
                    message_id: "message.newer".to_owned(),
                    metadata_json: None,
                    tool_names: None,
                },
            ],
            summary_nodes: vec![DashboardLcmCanonicalSummaryV1 {
                node_id: "summary.one".to_owned(),
                session_id: "session.newer".to_owned(),
                depth: 2,
                token_count: Some(5),
                source_token_count: Some(20),
                latest_at: Some(86_400),
                created_at: 86_400,
                expand_hint: "expand".to_owned(),
                summary: "summary".to_owned(),
            }],
            overview_matches: None,
            stats: DashboardLcmCanonicalStatsV1::default(),
            has_more: false,
            next_cursor: None,
        }
    }

    #[test]
    fn canonical_message_uses_unknown_model_o200k_only_when_available() {
        let message = message_json(DashboardLcmCanonicalMessageV1 {
            session_id: "session.message".to_owned(),
            provider: "codex".to_owned(),
            role: "assistant".to_owned(),
            timestamp: Some(1),
            ordinal: 1,
            content: "content whose tokenizer is unknown".to_owned(),
            message_id: "message.one".to_owned(),
            metadata_json: None,
            tool_names: None,
        });

        #[cfg(feature = "token-counting")]
        {
            assert!(message["token_count"].as_i64().is_some());
            assert_eq!(
                message["token_count_provenance"],
                serde_json::json!("o200k_approximate")
            );
        }
        #[cfg(not(feature = "token-counting"))]
        {
            assert!(message["token_count"].is_null());
            assert!(message["token_count_provenance"].is_null());
        }
    }

    #[test]
    fn native_usage_does_not_claim_visible_content_tokens() {
        let message = message_json(DashboardLcmCanonicalMessageV1 {
            session_id: "session.message".to_owned(),
            provider: "codex".to_owned(),
            role: "assistant".to_owned(),
            timestamp: Some(1),
            ordinal: 1,
            content: "short visible answer".to_owned(),
            message_id: "message.one".to_owned(),
            metadata_json: Some(
                serde_json::json!({
                    "usage": {
                        "input_tokens": 4_000,
                        "output_tokens": 999_999,
                        "completion_tokens": 888_888,
                        "total_tokens": 1_003_999
                    }
                })
                .to_string(),
            ),
            tool_names: None,
        });

        #[cfg(feature = "token-counting")]
        {
            assert_ne!(message["token_count"], 999_999);
            assert_ne!(message["token_count"], 888_888);
            assert_eq!(message["token_count_provenance"], "o200k_approximate");
        }
        #[cfg(not(feature = "token-counting"))]
        {
            assert!(message["token_count"].is_null());
            assert!(message["token_count_provenance"].is_null());
        }
    }

    #[test]
    fn optional_search_filters_are_trimmed_and_invalid_times_are_rejected() {
        assert_eq!(
            trimmed_nonempty("  assistant  ".to_owned()).as_deref(),
            Some("assistant")
        );
        assert_eq!(trimmed_nonempty(" \t ".to_owned()), None);
        assert_eq!(parse_optional_i64(" 42 "), Ok(Some(42)));
        assert_eq!(parse_optional_i64(" \t "), Ok(None));
        assert_eq!(parse_optional_i64("tomorrow"), Err(()));
    }

    #[test]
    fn overview_reduction_preserves_exact_counts_and_deterministic_recency() {
        let value = overview_json(aggregate_page(), String::new(), 1, "profile_sharded").expect("valid aggregate");

        assert_eq!(value["overview"]["messages_total"], 2);
        assert_eq!(value["overview"]["sessions_total"], 2);
        assert_eq!(value["overview"]["summary_nodes_total"], 1);
        assert_eq!(value["overview"]["compression"]["ratio"], 4.0);
        assert_eq!(value["latest_sessions"][0]["session_id"], "session.newer");
        assert_eq!(value["latest_sessions"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn timeline_reduction_uses_utc_buckets_and_reports_the_omitted_boundary() {
        let mut page = aggregate_page();
        page.messages[1].metadata_json =
            Some(r#"{"usage":{"input_tokens":900,"output_tokens":8}}"#.to_owned());
        let value = timeline_json(page, DashboardLcmTimelineBucketV1::Day, None, 1, "profile_sharded");

        assert_eq!(value["buckets"][0]["bucket"], "1970-01-02");
        #[cfg(feature = "token-counting")]
        {
            assert!(value["buckets"][0]["token_count"].as_i64().is_some());
            assert_eq!(
                value["buckets"][0]["token_count_provenance"],
                "o200k_approximate"
            );
            assert_eq!(value["buckets"][0]["known_message_count"], 1);
            assert_eq!(value["buckets"][0]["unknown_message_count"], 0);
        }
        #[cfg(not(feature = "token-counting"))]
        {
            assert!(value["buckets"][0]["token_count"].is_null());
            assert_eq!(value["buckets"][0]["token_count_provenance"], "unavailable");
            assert_eq!(value["buckets"][0]["known_message_count"], 0);
            assert_eq!(value["buckets"][0]["unknown_message_count"], 1);
        }
        assert_eq!(value["coverage"]["total_dated_buckets"], 2);
        assert_eq!(value["coverage"]["returned_buckets"], 1);
        assert_eq!(value["coverage"]["truncated"], true);
        assert_eq!(value["coverage"]["next_before_bucket"], "1970-01-02");
    }

    #[cfg(not(feature = "token-counting"))]
    #[test]
    fn timeline_does_not_publish_a_partial_total_when_any_message_is_unknown() {
        let mut page = aggregate_page();
        page.messages[0].timestamp = Some(86_400);
        page.messages[0].role = "assistant".to_owned();
        page.messages[0].metadata_json = Some(r#"{"usage":{"output_tokens":91}}"#.to_owned());

        let value = timeline_json(page, DashboardLcmTimelineBucketV1::Day, None, 25, "profile_sharded");
        let bucket = &value["buckets"][0];
        assert_eq!(bucket["count"], 2);
        assert!(bucket["token_count"].is_null());
        assert_eq!(bucket["token_count_provenance"], "unavailable");
        assert_eq!(bucket["known_message_count"], 0);
        assert_eq!(bucket["unknown_message_count"], 2);
    }

    #[cfg(feature = "token-counting")]
    #[test]
    fn timeline_marks_complete_visible_content_counts_as_o200k_approximate() {
        let mut page = aggregate_page();
        page.messages[0].timestamp = Some(86_400);
        page.messages[0].role = "assistant".to_owned();
        page.messages[0].metadata_json = Some(r#"{"usage":{"output_tokens":91}}"#.to_owned());

        let value = timeline_json(page, DashboardLcmTimelineBucketV1::Day, None, 25, "profile_sharded");
        let bucket = &value["buckets"][0];
        assert!(bucket["token_count"].as_i64().is_some());
        assert_eq!(bucket["token_count_provenance"], "o200k_approximate");
        assert_eq!(bucket["known_message_count"], 2);
        assert_eq!(bucket["unknown_message_count"], 0);
    }

    #[test]
    fn overview_reports_missing_canonical_summary_accounting_without_inventing_counts() {
        let mut page = aggregate_page();
        page.summary_nodes[0].source_token_count = None;

        let value =
            overview_json(page, String::new(), 25, "profile_sharded").expect("other overview fields remain valid");
        assert!(value["overview"]["compression"]["source_token_count"].is_null());
        assert!(value["overview"]["compression"]["ratio"].is_null());
    }
}

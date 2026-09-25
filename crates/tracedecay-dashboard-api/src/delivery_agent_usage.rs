//! Per-agent session usage under the pull request a checkout's branch carries.
//!
//! The session-Git correlation index names the sessions that worked on the
//! live branch; the project session store attributes each to its agent and
//! counts the tool invocations it recorded; the canonical provider-usage
//! projection supplies the tokens the provider reported. Nothing here is
//! estimated from message text, and a session the usage projection never saw
//! stays counted without tokens rather than as zero.

use std::collections::BTreeMap;
use std::path::Path;

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};
use tracedecay_automation_runtime::automation::host_io::HostIo;
use tracedecay_domain::ObservationScopeV1;
use tracedecay_global_db::{GlobalDbGitCorrelationStore, RegisteredGlobalDb};
use tracedecay_runtime_core::db::engine::params;
use tracedecay_session_memory::provider_usage::{
    AggregatedProviderUsageCountersV1, ProviderUsageCoverageV1, ProviderUsageSessionTotalsV1,
    provider_usage_aggregate, provider_usage_by_session,
};
use tracedecay_sessions::runtime::git_correlation::{
    CommitRelationFilter, GitCorrelationError, GitRefFilter, MAX_SESSIONS_FOR_LIMIT,
    SessionsForQuery,
};

use super::DashboardState;
use super::analytics_api::managed_agent_label_for_session;
use super::delivery_api::{DeliveryGitHeadV1, DeliveryGitStatusV1, DeliveryProjectionV1};
use super::util::{i64_field, query_rows, str_field};

const CORRELATION_AUTHORITY: &str = "session-Git correlation index";
const SESSION_STORE_AUTHORITY: &str = "registered project session store";

/// The agents whose sessions the correlation index places on `branch`.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryAgentUsageV1 {
    /// The live branch the correlation index was queried with.
    pub branch: String,
    /// Correlated sessions this project's session store holds; the
    /// denominator every row's `sessions` sums to.
    pub sessions: u64,
    /// The correlation read reached its session ceiling, so further sessions
    /// on this branch may exist.
    pub truncated: bool,
    /// Coverage of the provider-usage read the token counters came from.
    pub usage_coverage: ProviderUsageCoverageV1,
    pub agents: Vec<DeliveryAgentUsageRowV1>,
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct DeliveryAgentUsageRowV1 {
    /// Managed-agent label, else the raw agent id; `None` groups the sessions
    /// that record no agent at all.
    pub agent: Option<String>,
    pub provider: String,
    pub sessions: u64,
    /// Sessions the provider-usage projection holds usage for.
    pub sessions_with_usage: u64,
    /// Every session in the row has usage and none of it was reduced with an
    /// issue; otherwise `counters` is a lower bound.
    pub usage_complete: bool,
    /// Summed over `sessions_with_usage`; a counter is `None` unless every one
    /// of those sessions reported it.
    pub counters: AggregatedProviderUsageCountersV1,
    /// Tool invocations the session store recorded for these sessions.
    pub tool_calls: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CorrelatedSessionV1 {
    provider: String,
    session_id: String,
    agent: Option<String>,
    tool_calls: u64,
}

pub(super) async fn agent_usage_projection(
    state: &DashboardState,
    changes: &DeliveryProjectionV1<DeliveryGitStatusV1>,
) -> DeliveryProjectionV1<DeliveryAgentUsageV1> {
    let branch = match changes {
        DeliveryProjectionV1::Ready { value } => match &value.head {
            DeliveryGitHeadV1::Attached { branch, .. } => branch.clone(),
            DeliveryGitHeadV1::Detached { commit } => {
                return DeliveryProjectionV1::unavailable(
                    CORRELATION_AUTHORITY,
                    format!("HEAD is detached at {commit}, so no branch names a pull request"),
                );
            }
            DeliveryGitHeadV1::Unborn { branch } => {
                return DeliveryProjectionV1::unavailable(
                    CORRELATION_AUTHORITY,
                    format!("branch {branch} has no commit yet"),
                );
            }
        },
        _ => {
            return DeliveryProjectionV1::unavailable(
                CORRELATION_AUTHORITY,
                "the live Git head could not be read, so no branch is known",
            );
        }
    };
    let (Some(db), Some(scope)) = (state.lcm_db.as_deref(), state.resolved_scope.as_ref()) else {
        return DeliveryProjectionV1::unavailable(
            SESSION_STORE_AUTHORITY,
            "no registered project session store or resolved project scope is mounted",
        );
    };
    let query = SessionsForQuery {
        git_ref: GitRefFilter::Branch(branch.clone()),
        since: None,
        until: None,
        limit: MAX_SESSIONS_FOR_LIMIT,
    };
    let (hits, presence) = match GlobalDbGitCorrelationStore::new(db)
        .sessions_for_with_relation_and_presence(&query, CommitRelationFilter::All)
        .await
    {
        Ok(read) => read,
        Err(GitCorrelationError::Unavailable(reason)) => {
            return DeliveryProjectionV1::unavailable(CORRELATION_AUTHORITY, reason);
        }
        Err(error) => {
            return DeliveryProjectionV1::unavailable(
                CORRELATION_AUTHORITY,
                format!("the correlation read failed: {error}"),
            );
        }
    };
    if !presence.projection_available || !presence.spans_present {
        return DeliveryProjectionV1::NotPublished {
            required_authority: CORRELATION_AUTHORITY.to_owned(),
            reason: "no session has recorded a Git branch span yet".to_owned(),
        };
    }
    let truncated = hits.len() >= MAX_SESSIONS_FOR_LIMIT;
    let pairs: Vec<Value> = hits
        .iter()
        .map(|hit| json!([hit.provider, hit.session_id]))
        .collect();
    let sessions = match project_sessions(
        db,
        &state.host_io,
        scope.project_id.as_str(),
        &state.project_root,
        &pairs,
    )
    .await
    {
        Ok(sessions) => sessions,
        Err(reason) => return DeliveryProjectionV1::unavailable(SESSION_STORE_AUTHORITY, reason),
    };
    let (usage, usage_coverage) = if sessions.is_empty() {
        (BTreeMap::new(), ProviderUsageCoverageV1::Complete)
    } else {
        let aggregate = provider_usage_aggregate(
            db,
            &ObservationScopeV1::Project {
                project_id: scope.project_id.clone(),
            },
            None,
            None,
        )
        .await;
        (provider_usage_by_session(&aggregate), aggregate.coverage)
    };
    agent_usage(branch, sessions, &usage, usage_coverage, truncated)
}

/// Resolves correlated `(provider, session_id)` pairs to this project's
/// sessions, with their agent and recorded tool invocations. A hit whose
/// provider is empty is an unattributed span and matches the session id alone.
async fn project_sessions(
    db: &RegisteredGlobalDb,
    host_io: &HostIo,
    project_id: &str,
    project_root: &Path,
    pairs: &[Value],
) -> Result<Vec<CorrelatedSessionV1>, String> {
    if pairs.is_empty() {
        return Ok(Vec::new());
    }
    let canonical = RegisteredGlobalDb::canonical_project_key(project_root);
    let opened = project_root.to_string_lossy().into_owned();
    let connection = db.read_connection();
    let rows = query_rows(
        &connection,
        "WITH correlated(provider, session_id) AS (
             SELECT json_extract(value, '$[0]'), json_extract(value, '$[1]')
             FROM json_each(?3)
         )
         SELECT DISTINCT s.provider,
                s.session_id,
                COALESCE(s.agent_id, '') AS agent_id,
                COALESCE(s.metadata_json, '') AS metadata_json,
                (SELECT COUNT(*) FROM lcm_raw_messages m
                  WHERE m.provider = s.provider
                    AND m.session_id = s.session_id
                    AND m.kind IN ('tool_call', 'file_edit')) AS tool_calls
         FROM correlated c
         JOIN sessions s
           ON s.session_id = c.session_id
          AND (c.provider = '' OR s.provider = c.provider)
         WHERE s.project_key IN (?1, ?2, ?4) OR s.project_path IN (?1, ?2)
         ORDER BY s.provider, s.session_id",
        params![
            canonical,
            opened,
            Value::Array(pairs.to_vec()).to_string(),
            project_id
        ],
    )
    .await
    .map_err(|error| format!("correlated session query failed: {error}"))?;
    Ok(rows
        .iter()
        .map(|row| {
            let agent_id = str_field(row, "agent_id");
            let agent =
                managed_agent_label_for_session(host_io, agent_id, str_field(row, "metadata_json"))
                    .map(str::to_owned)
                    .or_else(|| (!agent_id.trim().is_empty()).then(|| agent_id.trim().to_owned()));
            CorrelatedSessionV1 {
                provider: str_field(row, "provider").to_owned(),
                session_id: str_field(row, "session_id").to_owned(),
                agent,
                tool_calls: u64::try_from(i64_field(row, "tool_calls")).unwrap_or(0),
            }
        })
        .collect())
}

fn agent_usage(
    branch: String,
    sessions: Vec<CorrelatedSessionV1>,
    usage: &BTreeMap<(String, String), ProviderUsageSessionTotalsV1>,
    usage_coverage: ProviderUsageCoverageV1,
    truncated: bool,
) -> DeliveryProjectionV1<DeliveryAgentUsageV1> {
    let session_count = sessions.len() as u64;
    let mut grouped =
        BTreeMap::<(Option<String>, String), Vec<(&CorrelatedSessionV1, Option<&_>)>>::new();
    for session in &sessions {
        grouped
            .entry((session.agent.clone(), session.provider.clone()))
            .or_default()
            .push((
                session,
                usage.get(&(session.provider.clone(), session.session_id.clone())),
            ));
    }
    let agents = grouped
        .into_iter()
        .map(|((agent, provider), members)| {
            let reported: Vec<&ProviderUsageSessionTotalsV1> =
                members.iter().filter_map(|(_, usage)| *usage).collect();
            let sum = |field: fn(&AggregatedProviderUsageCountersV1) -> Option<u64>| {
                reported
                    .iter()
                    .try_fold(0u64, |total, usage| {
                        total.checked_add(field(&usage.counters)?)
                    })
                    .filter(|_| !reported.is_empty())
            };
            DeliveryAgentUsageRowV1 {
                agent,
                provider,
                sessions: members.len() as u64,
                sessions_with_usage: reported.len() as u64,
                usage_complete: reported.len() == members.len()
                    && reported.iter().all(|usage| usage.complete),
                counters: AggregatedProviderUsageCountersV1 {
                    input_tokens: sum(|counters| counters.input_tokens),
                    output_tokens: sum(|counters| counters.output_tokens),
                    cache_read_tokens: sum(|counters| counters.cache_read_tokens),
                    cache_write_tokens: sum(|counters| counters.cache_write_tokens),
                    reasoning_tokens: sum(|counters| counters.reasoning_tokens),
                    total_tokens: sum(|counters| counters.total_tokens),
                },
                tool_calls: members.iter().map(|(session, _)| session.tool_calls).sum(),
            }
        })
        .collect::<Vec<_>>();
    let value = DeliveryAgentUsageV1 {
        branch,
        sessions: session_count,
        truncated,
        usage_coverage,
        agents,
    };
    if value.sessions == 0 && !truncated {
        DeliveryProjectionV1::EmptyMeasured { value }
    } else if truncated || usage_coverage != ProviderUsageCoverageV1::Complete {
        DeliveryProjectionV1::Partial { value }
    } else {
        DeliveryProjectionV1::Ready { value }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(
        provider: &str,
        id: &str,
        agent: Option<&str>,
        tool_calls: u64,
    ) -> CorrelatedSessionV1 {
        CorrelatedSessionV1 {
            provider: provider.to_owned(),
            session_id: id.to_owned(),
            agent: agent.map(str::to_owned),
            tool_calls,
        }
    }

    fn reported(
        input: u64,
        output: u64,
        total: Option<u64>,
        complete: bool,
    ) -> ProviderUsageSessionTotalsV1 {
        ProviderUsageSessionTotalsV1 {
            usage_events: 1,
            counters: AggregatedProviderUsageCountersV1 {
                input_tokens: Some(input),
                output_tokens: Some(output),
                total_tokens: total,
                ..AggregatedProviderUsageCountersV1::unknown()
            },
            complete,
        }
    }

    fn rows(projection: &DeliveryProjectionV1<DeliveryAgentUsageV1>) -> &DeliveryAgentUsageV1 {
        match projection {
            DeliveryProjectionV1::Ready { value }
            | DeliveryProjectionV1::Partial { value }
            | DeliveryProjectionV1::EmptyMeasured { value } => value,
            other => panic!("unexpected projection {other:?}"),
        }
    }

    #[test]
    fn sums_reported_usage_and_tool_calls_per_agent() {
        let usage = BTreeMap::from([
            (
                ("claude".to_owned(), "a1".to_owned()),
                reported(100, 40, Some(140), true),
            ),
            (
                ("claude".to_owned(), "a2".to_owned()),
                reported(10, 5, Some(15), true),
            ),
        ]);
        let projection = agent_usage(
            "feat/x".to_owned(),
            vec![
                session("claude", "a1", Some("planner"), 3),
                session("claude", "a2", Some("planner"), 2),
                session("claude", "b1", None, 7),
            ],
            &usage,
            ProviderUsageCoverageV1::Complete,
            false,
        );
        assert!(matches!(projection, DeliveryProjectionV1::Ready { .. }));
        let value = rows(&projection);
        assert_eq!(value.sessions, 3);
        let planner = value
            .agents
            .iter()
            .find(|row| row.agent.as_deref() == Some("planner"))
            .unwrap();
        assert_eq!(planner.sessions, 2);
        assert_eq!(planner.tool_calls, 5);
        assert_eq!(planner.counters.total_tokens, Some(155));
        assert_eq!(planner.counters.input_tokens, Some(110));
        assert!(planner.usage_complete);
        // A session the projection never saw keeps its tool calls but no
        // invented token count.
        let unlabeled = value.agents.iter().find(|row| row.agent.is_none()).unwrap();
        assert_eq!(unlabeled.tool_calls, 7);
        assert_eq!(unlabeled.sessions_with_usage, 0);
        assert_eq!(unlabeled.counters.total_tokens, None);
        assert!(!unlabeled.usage_complete);
    }

    #[test]
    fn a_counter_one_session_omits_is_unknown_not_a_partial_sum() {
        let usage = BTreeMap::from([
            (
                ("claude".to_owned(), "a1".to_owned()),
                reported(100, 40, Some(140), true),
            ),
            (
                ("claude".to_owned(), "a2".to_owned()),
                reported(10, 5, None, false),
            ),
        ]);
        let projection = agent_usage(
            "feat/x".to_owned(),
            vec![
                session("claude", "a1", Some("planner"), 0),
                session("claude", "a2", Some("planner"), 0),
            ],
            &usage,
            ProviderUsageCoverageV1::Complete,
            false,
        );
        let row = &rows(&projection).agents[0];
        assert_eq!(row.counters.total_tokens, None);
        assert_eq!(row.counters.output_tokens, Some(45));
        assert!(
            !row.usage_complete,
            "an issue-bearing session makes the sums a lower bound"
        );
    }

    #[test]
    fn typed_states_follow_coverage_and_truncation() {
        let empty = agent_usage(
            "feat/x".to_owned(),
            Vec::new(),
            &BTreeMap::new(),
            ProviderUsageCoverageV1::Complete,
            false,
        );
        assert!(matches!(empty, DeliveryProjectionV1::EmptyMeasured { .. }));
        let unavailable_usage = agent_usage(
            "feat/x".to_owned(),
            vec![session("claude", "a1", None, 1)],
            &BTreeMap::new(),
            ProviderUsageCoverageV1::Unavailable,
            false,
        );
        assert!(matches!(
            unavailable_usage,
            DeliveryProjectionV1::Partial { .. }
        ));
        let truncated = agent_usage(
            "feat/x".to_owned(),
            vec![session("claude", "a1", None, 1)],
            &BTreeMap::new(),
            ProviderUsageCoverageV1::Complete,
            true,
        );
        assert!(matches!(truncated, DeliveryProjectionV1::Partial { .. }));
    }
}

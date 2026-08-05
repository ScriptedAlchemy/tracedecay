//! Daemon-owned accounting and analytics authority.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay_application::{
    AccountingAuthorityPort, AccountingFuture, AccountingIngestGuaranteeV1, AccountingInvocationV1,
    AccountingOperationV1, AccountingOutcomeV1, AccountingProjectScopeV1, AccountingResponseV1,
    AccountingScopeV1, AccountingSourceCoverageV1, AccountingSourceStateV1, AccountingSourceV1,
    CancellationObservation, CancellationSignal, CancellationStage, Deadline, OperationBudgetUsage,
    OperationReceipt, OperationTermination,
};
use tracedecay_domain::{ProjectId, UserProfileId, UtcMicros};

use crate::global_db::RegisteredGlobalDb;
use crate::tracedecay::TraceDecay;

#[derive(Clone)]
struct ProjectAccountingAuthority {
    scope: AccountingProjectScopeV1,
    root: PathBuf,
    graph: Arc<TraceDecay>,
}

/// One profile database authority with an optional exact project scope.
pub(crate) struct DaemonAccountingAuthority {
    profile_id: UserProfileId,
    accounting: Arc<RegisteredGlobalDb>,
    profile_root: PathBuf,
    transcript_source_home: Option<PathBuf>,
    project: Option<ProjectAccountingAuthority>,
    project_sessions: Option<Arc<RegisteredGlobalDb>>,
    profile_sessions: Option<Arc<RegisteredGlobalDb>>,
}

pub(crate) struct DaemonAccountingOwners {
    pub(crate) profile_id: UserProfileId,
    pub(crate) accounting: Arc<RegisteredGlobalDb>,
    pub(crate) profile_root: PathBuf,
    pub(crate) transcript_source_home: Option<PathBuf>,
    pub(crate) project_id: Option<ProjectId>,
    pub(crate) project_root: Option<PathBuf>,
    pub(crate) graph: Option<Arc<TraceDecay>>,
    pub(crate) project_sessions: Option<Arc<RegisteredGlobalDb>>,
    pub(crate) profile_sessions: Option<Arc<RegisteredGlobalDb>>,
}

impl DaemonAccountingAuthority {
    pub(crate) fn for_project(
        profile_identity: &super::profile_identity::LocalProfileIdentityAuthorityV1,
        accounting: Arc<RegisteredGlobalDb>,
        transcript_source_home: Option<PathBuf>,
        graph: Arc<TraceDecay>,
        project_sessions: Option<Arc<RegisteredGlobalDb>>,
        profile_sessions: Option<Arc<RegisteredGlobalDb>>,
    ) -> Option<Arc<dyn AccountingAuthorityPort>> {
        let project_id = graph
            .store_layout()
            .identity
            .project_id
            .as_deref()
            .and_then(|value| ProjectId::new(value.to_owned()).ok())?;
        let project_root = graph.project_root().to_path_buf();
        Self::new(DaemonAccountingOwners {
            profile_id: profile_identity.profile_id().clone(),
            accounting,
            profile_root: profile_identity.profile_root().to_path_buf(),
            transcript_source_home,
            project_id: Some(project_id),
            project_root: Some(project_root),
            graph: Some(graph),
            project_sessions,
            profile_sessions,
        })
        .map(|authority| Arc::new(authority) as Arc<dyn AccountingAuthorityPort>)
    }

    pub(crate) fn new(owners: DaemonAccountingOwners) -> Option<Self> {
        let DaemonAccountingOwners {
            profile_id,
            accounting,
            profile_root,
            transcript_source_home,
            project_id,
            project_root,
            graph,
            project_sessions,
            profile_sessions,
        } = owners;
        let project = match (project_id, project_root, graph) {
            (Some(project_id), Some(root), Some(graph)) => Some(ProjectAccountingAuthority {
                scope: AccountingProjectScopeV1 {
                    project_id,
                    canonical_project_key: RegisteredGlobalDb::canonical_project_key(&root),
                },
                root,
                graph,
            }),
            (None, None, None) => None,
            _ => return None,
        };
        Some(Self {
            profile_id,
            accounting,
            profile_root,
            transcript_source_home,
            project,
            project_sessions,
            profile_sessions,
        })
    }

    fn scope(&self, include_project: bool) -> AccountingScopeV1 {
        AccountingScopeV1 {
            profile_id: self.profile_id.clone(),
            project: include_project
                .then(|| self.project.as_ref().map(|project| project.scope.clone()))
                .flatten(),
        }
    }

    async fn execute(
        &self,
        operation: AccountingOperationV1,
    ) -> Result<ServedAccountingPayload, String> {
        match operation {
            AccountingOperationV1::CostSummary { range } => self.cost_summary(range).await,
            AccountingOperationV1::AnalyticsSync => self.analytics_sync().await,
            AccountingOperationV1::AnalyticsDiagnostics {
                all_projects,
                no_sync,
            } => self.analytics_diagnostics(all_projects, no_sync).await,
            AccountingOperationV1::StatusAccounting => self.status_accounting().await,
        }
    }

    async fn cost_summary(&self, range: String) -> Result<ServedAccountingPayload, String> {
        crate::accounting::pricing::refresh_if_stale();
        let ingest = match self.transcript_source_home.as_deref() {
            Some(home) => Some(crate::accounting::parser::ingest_at(&self.accounting, home).await),
            None => None,
        };
        let observed_at = tracedecay_application::clock::now_micros();
        let now_epoch = u64::try_from(observed_at.0 / 1_000_000)
            .map_err(|error| format!("accounting clock is before the Unix epoch: {error}"))?;
        let since = crate::accounting::metrics::parse_range_at(&range, now_epoch)?;
        let tokens_saved = self.accounting.try_global_tokens_saved().await?;
        let summary =
            crate::accounting::metrics::cost_summary(&self.accounting, since, tokens_saved).await?;
        let today_since = crate::accounting::metrics::parse_range_at("today", now_epoch)?;
        let today_cost = self.accounting.try_total_cost_since(today_since).await?;
        let today_breakdown = self
            .accounting
            .try_token_breakdown_since(today_since)
            .await?;
        let costs = crate::application::observability::costs_read_model(
            &self.accounting,
            None,
            since as i64,
        )
        .await;
        let (transcript_state, transcript_reason) = match ingest.as_ref() {
            Some(ingest) if ingest.sources_failed == 0 => (AccountingSourceStateV1::Complete, None),
            Some(ingest) => (
                AccountingSourceStateV1::Partial,
                Some(format!(
                    "{} transcript source(s) failed",
                    ingest.sources_failed
                )),
            ),
            None => (
                AccountingSourceStateV1::Unavailable,
                Some("daemon transcript source authority is unavailable".to_owned()),
            ),
        };
        Ok(ServedAccountingPayload {
            payload: json!({
                "range": range,
                "ingest": ingest.as_ref().map(|ingest| json!({
                    "turns_inserted": ingest.turns_inserted,
                    "cost_usd": ingest.cost_usd,
                    "tokens_consumed": ingest.tokens_consumed,
                    "sources_failed": ingest.sources_failed,
                })),
                "summary": {
                    "total_cost": summary.total_cost,
                    "total_input_tokens": summary.total_input_tokens,
                    "total_output_tokens": summary.total_output_tokens,
                    "total_cache_read_tokens": summary.total_cache_read_tokens,
                    "by_model": summary.by_model,
                    "by_category": summary.by_category,
                    "tokens_saved": summary.tokens_saved,
                    "efficiency_ratio": summary.efficiency_ratio,
                },
                "today": {
                    "cost": today_cost,
                    "input_tokens": today_breakdown.0,
                    "output_tokens": today_breakdown.1,
                    "cache_read_tokens": today_breakdown.2,
                },
                "costs": costs,
            }),
            scope: self.scope(false),
            coverage: vec![
                source_coverage(
                    AccountingSourceV1::AccountingLedger,
                    AccountingSourceStateV1::Complete,
                    None,
                ),
                source_coverage(
                    AccountingSourceV1::TurnTranscript,
                    transcript_state,
                    transcript_reason,
                ),
            ],
            ingest_guarantee: AccountingIngestGuaranteeV1::DurableSourceCursor,
        })
    }

    async fn analytics_sync(&self) -> Result<ServedAccountingPayload, String> {
        let hook_sources = self.hook_import_sources(false).await?;
        let outcome =
            crate::analytics_bridge::analytics_sync_with_db(&self.accounting, hook_sources).await;
        let failed = import_failure_count(&outcome);
        Ok(ServedAccountingPayload {
            payload: outcome,
            scope: self.scope(self.project.is_some()),
            coverage: vec![source_coverage(
                AccountingSourceV1::HookAnalytics,
                if failed == 0 {
                    AccountingSourceStateV1::Complete
                } else {
                    AccountingSourceStateV1::Partial
                },
                (failed > 0).then(|| format!("{failed} hook analytics source(s) failed")),
            )],
            ingest_guarantee: AccountingIngestGuaranteeV1::DurableSourceCursor,
        })
    }

    async fn analytics_diagnostics(
        &self,
        all_projects: bool,
        no_sync: bool,
    ) -> Result<ServedAccountingPayload, String> {
        let project_root = self.project.as_ref().map(|project| project.root.as_path());
        let project_store_root = self
            .project
            .as_ref()
            .map(|project| project.graph.store_layout().data_root.as_path());
        if !all_projects && project_root.is_some() && self.project_sessions.is_none() {
            return Err("registered project session authority is unavailable".to_owned());
        }
        if all_projects && self.profile_sessions.is_none() {
            return Err("registered profile session authority is unavailable".to_owned());
        }
        if !all_projects && self.project.is_none() {
            return Err("registered project accounting scope is unavailable".to_owned());
        }
        let hook_sources = self.hook_import_sources(all_projects).await?;
        let payload = crate::analytics_bridge::analytics_diagnostics_with_db(
            &self.accounting,
            self.project_sessions.as_deref(),
            self.profile_sessions.as_deref(),
            project_root,
            project_store_root,
            hook_sources,
            all_projects,
            no_sync,
        )
        .await
        .map_err(|error| error.to_string())?;
        let failed = payload
            .get("import")
            .filter(|import| !import.is_null())
            .map_or(0, import_failure_count);
        let mut coverage = vec![
            source_coverage(
                AccountingSourceV1::AccountingLedger,
                AccountingSourceStateV1::Complete,
                None,
            ),
            source_coverage(
                AccountingSourceV1::HookAnalytics,
                if no_sync {
                    AccountingSourceStateV1::NotRequested
                } else if failed == 0 {
                    AccountingSourceStateV1::Complete
                } else {
                    AccountingSourceStateV1::Partial
                },
                (failed > 0).then(|| format!("{failed} hook analytics source(s) failed")),
            ),
        ];
        coverage.push(source_coverage(
            if all_projects {
                AccountingSourceV1::ProfileSessions
            } else {
                AccountingSourceV1::ProjectSessions
            },
            AccountingSourceStateV1::Complete,
            None,
        ));
        Ok(ServedAccountingPayload {
            payload,
            scope: self.scope(!all_projects && self.project.is_some()),
            coverage,
            ingest_guarantee: if no_sync {
                AccountingIngestGuaranteeV1::NotApplicable
            } else {
                AccountingIngestGuaranteeV1::DurableSourceCursor
            },
        })
    }

    async fn status_accounting(&self) -> Result<ServedAccountingPayload, String> {
        let project = self
            .project
            .as_ref()
            .ok_or_else(|| "project accounting authority is unavailable".to_owned())?;
        let tokens_saved = project
            .graph
            .get_tokens_saved()
            .await
            .map_err(|error| error.to_string())?;
        self.accounting
            .try_upsert_project_tokens(&project.root, tokens_saved)
            .await
            .map_err(|error| error.to_string())?;
        let global_tokens_saved = self.accounting.try_global_tokens_saved().await?;
        let outside_project_tokens = global_tokens_saved.saturating_sub(tokens_saved);
        Ok(ServedAccountingPayload {
            payload: json!({
                "tokens_saved": tokens_saved,
                "global_tokens_saved": (outside_project_tokens > 0)
                    .then_some(outside_project_tokens),
            }),
            scope: self.scope(true),
            coverage: vec![source_coverage(
                AccountingSourceV1::AccountingLedger,
                AccountingSourceStateV1::Complete,
                None,
            )],
            ingest_guarantee: AccountingIngestGuaranteeV1::NotApplicable,
        })
    }

    async fn hook_import_sources(
        &self,
        include_all_projects: bool,
    ) -> Result<Vec<crate::analytics_bridge::HookImportSource>, String> {
        let mut sources = Vec::new();
        if let Some(project) = self.project.as_ref() {
            sources.push(crate::analytics_bridge::HookImportSource {
                path: project
                    .graph
                    .store_layout()
                    .data_root
                    .join("hook_analytics.jsonl"),
                default_project_root: Some(project.root.clone()),
            });
        }
        if include_all_projects {
            let mut after_project_id = None;
            loop {
                let projects = self
                    .accounting
                    .list_code_projects_after(after_project_id.as_deref(), 256)
                    .await
                    .map_err(|error| {
                        format!("registered accounting project scope is unavailable: {error}")
                    })?;
                if projects.is_empty() {
                    break;
                }
                after_project_id = projects.last().map(|project| project.project_id.clone());
                for registered in projects {
                    let root = PathBuf::from(&registered.canonical_root);
                    let layout = crate::storage::resolve_layout(&root, &self.profile_root)
                        .map_err(|error| {
                            format!(
                                "registered accounting project '{}' has no exact store authority: {error}",
                                registered.project_id
                            )
                        })?;
                    let path = layout.data_root.join("hook_analytics.jsonl");
                    if !sources.iter().any(|source| source.path == path) {
                        sources.push(crate::analytics_bridge::HookImportSource {
                            path,
                            default_project_root: Some(root),
                        });
                    }
                }
            }
        }
        let profile_path = self.profile_root.join("hook_analytics.jsonl");
        if !sources.iter().any(|source| source.path == profile_path) {
            sources.push(crate::analytics_bridge::HookImportSource {
                path: profile_path,
                default_project_root: None,
            });
        }
        Ok(sources)
    }
}

impl AccountingAuthorityPort for DaemonAccountingAuthority {
    fn invoke<'a>(&'a self, invocation: AccountingInvocationV1) -> AccountingFuture<'a> {
        Box::pin(async move {
            let include_project = matches!(
                &invocation.operation,
                AccountingOperationV1::StatusAccounting
                    | AccountingOperationV1::AnalyticsDiagnostics {
                        all_projects: false,
                        ..
                    }
            );
            let unavailable_scope = self.scope(include_project);
            let started_at = tracedecay_application::clock::now_micros();
            if invocation.cancellation.is_cancelled() {
                return terminal_without_payload(
                    unavailable_scope,
                    &invocation,
                    started_at,
                    started_at,
                    OperationTermination::Cancelled,
                );
            }
            let Some(remaining) = remaining_until(&invocation.deadline, started_at) else {
                return terminal_without_payload(
                    unavailable_scope,
                    &invocation,
                    started_at,
                    started_at,
                    OperationTermination::TimedOut,
                );
            };
            let operation_name = invocation.operation.name();
            let execution = self.execute(invocation.operation.clone());
            let result =
                await_controlled(execution, remaining, invocation.cancellation.clone()).await;
            let observed_end = tracedecay_application::clock::now_micros();
            let ended_at = UtcMicros(observed_end.0.max(started_at.0));
            match result {
                Controlled::Completed(Ok(served)) => {
                    let partial = served.coverage.iter().any(|coverage| {
                        matches!(
                            coverage.state,
                            AccountingSourceStateV1::Partial | AccountingSourceStateV1::Unavailable
                        )
                    });
                    let termination = if partial {
                        OperationTermination::Partial
                    } else {
                        OperationTermination::Completed
                    };
                    let receipt = receipt(&invocation, started_at, ended_at, termination);
                    let response = AccountingResponseV1 {
                        scope: served.scope,
                        payload: served.payload,
                        coverage: served.coverage,
                        ingest_guarantee: served.ingest_guarantee,
                        receipt,
                    };
                    if partial {
                        AccountingOutcomeV1::Partial(response)
                    } else {
                        AccountingOutcomeV1::Complete(response)
                    }
                }
                Controlled::Completed(Err(reason)) => unavailable(
                    unavailable_scope,
                    format!("{operation_name} unavailable: {reason}"),
                    AccountingSourceV1::AccountingLedger,
                    &invocation,
                    started_at,
                    ended_at,
                ),
                Controlled::Cancelled => terminal_without_payload(
                    unavailable_scope,
                    &invocation,
                    started_at,
                    ended_at,
                    OperationTermination::Cancelled,
                ),
                Controlled::TimedOut => terminal_without_payload(
                    unavailable_scope,
                    &invocation,
                    started_at,
                    ended_at,
                    OperationTermination::TimedOut,
                ),
            }
        })
    }
}

struct ServedAccountingPayload {
    payload: Value,
    scope: AccountingScopeV1,
    coverage: Vec<AccountingSourceCoverageV1>,
    ingest_guarantee: AccountingIngestGuaranteeV1,
}

enum Controlled<T> {
    Completed(T),
    Cancelled,
    TimedOut,
}

async fn await_controlled<F, T>(
    future: F,
    remaining: Duration,
    cancellation: CancellationSignal,
) -> Controlled<T>
where
    F: Future<Output = T>,
{
    tokio::pin!(future);
    let deadline = tokio::time::sleep(remaining);
    tokio::pin!(deadline);
    let cancelled = crate::daemon_client::wait_for_cancellation(cancellation);
    tokio::pin!(cancelled);
    tokio::select! {
        output = &mut future => Controlled::Completed(output),
        () = &mut cancelled => Controlled::Cancelled,
        () = &mut deadline => Controlled::TimedOut,
    }
}

fn remaining_until(deadline: &Deadline, observed_at: UtcMicros) -> Option<Duration> {
    let micros = deadline.expires_at.0.checked_sub(observed_at.0)?;
    (micros > 0).then(|| Duration::from_micros(micros as u64))
}

fn receipt(
    invocation: &AccountingInvocationV1,
    started_at: UtcMicros,
    ended_at: UtcMicros,
    termination: OperationTermination,
) -> OperationReceipt {
    let cancellation = matches!(
        termination,
        OperationTermination::Cancelled | OperationTermination::TimedOut
    )
    .then(|| CancellationObservation {
        stage: CancellationStage::DuringRead,
        observed_at: ended_at,
    });
    OperationReceipt {
        started_at,
        ended_at,
        effective_deadline: invocation.deadline.clone(),
        cancellation,
        budget: OperationBudgetUsage {
            units_consumed: 1,
            bytes_consumed: 0,
            elapsed_micros: ended_at.0.saturating_sub(started_at.0) as u64,
        },
        termination,
    }
}

fn terminal_without_payload(
    scope: AccountingScopeV1,
    invocation: &AccountingInvocationV1,
    started_at: UtcMicros,
    observed_at: UtcMicros,
    termination: OperationTermination,
) -> AccountingOutcomeV1 {
    let receipt = receipt(invocation, started_at, observed_at, termination);
    match termination {
        OperationTermination::Cancelled => AccountingOutcomeV1::Cancelled { scope, receipt },
        OperationTermination::TimedOut => AccountingOutcomeV1::TimedOut { scope, receipt },
        _ => unavailable(
            scope,
            "invalid accounting terminal state",
            AccountingSourceV1::AccountingLedger,
            invocation,
            started_at,
            observed_at,
        ),
    }
}

fn unavailable(
    scope: AccountingScopeV1,
    reason: impl Into<String>,
    source: AccountingSourceV1,
    invocation: &AccountingInvocationV1,
    started_at: UtcMicros,
    ended_at: UtcMicros,
) -> AccountingOutcomeV1 {
    let reason = reason.into();
    AccountingOutcomeV1::Unavailable {
        scope,
        coverage: vec![source_coverage(
            source,
            AccountingSourceStateV1::Unavailable,
            Some(reason.clone()),
        )],
        reason,
        receipt: receipt(
            invocation,
            started_at,
            ended_at,
            OperationTermination::Unavailable,
        ),
    }
}

fn source_coverage(
    source: AccountingSourceV1,
    state: AccountingSourceStateV1,
    reason: Option<String>,
) -> AccountingSourceCoverageV1 {
    AccountingSourceCoverageV1 {
        source,
        state,
        reason,
    }
}

fn import_failure_count(import: &Value) -> usize {
    import
        .get("sources")
        .and_then(Value::as_array)
        .map_or(0, |sources| {
            sources
                .iter()
                .filter(|source| source.get("error").is_some_and(|error| !error.is_null()))
                .count()
        })
}

#[cfg(test)]
mod tests {
    use tracedecay_application::{
        AccountingInvocationV1, AccountingOperationV1, AccountingOutcomeV1, AccountingScopeV1,
        CancellationSignal, Deadline, OperationTermination, RequestId,
    };
    use tracedecay_domain::{UserProfileId, UtcMicros};

    use super::terminal_without_payload;

    fn invocation() -> AccountingInvocationV1 {
        AccountingInvocationV1 {
            request_id: RequestId::new("request.accounting.receipt").expect("request id"),
            deadline: Deadline::new(UtcMicros(100)).expect("deadline"),
            cancellation: CancellationSignal::active("cancel.accounting.receipt")
                .expect("cancellation"),
            operation: AccountingOperationV1::AnalyticsSync,
        }
    }

    fn scope() -> AccountingScopeV1 {
        AccountingScopeV1 {
            profile_id: UserProfileId::new("profile.accounting.receipt").expect("profile id"),
            project: None,
        }
    }

    #[test]
    fn terminal_accounting_outcomes_retain_original_start_and_receipt() {
        let outcome = terminal_without_payload(
            scope(),
            &invocation(),
            UtcMicros(10),
            UtcMicros(20),
            OperationTermination::TimedOut,
        );
        let AccountingOutcomeV1::TimedOut { receipt, .. } = outcome else {
            panic!("expected timed-out accounting outcome");
        };
        assert_eq!(receipt.started_at, UtcMicros(10));
        assert_eq!(receipt.ended_at, UtcMicros(20));
        assert_eq!(receipt.termination, OperationTermination::TimedOut);
        receipt.validate().expect("valid terminal receipt");
    }
}

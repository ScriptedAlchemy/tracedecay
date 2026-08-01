//! Integration, workflow, observation, and temporal fixture adapters.

use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tracedecay_domain::{
    LocatorDigest, ObservationScopeV1, ObservationSourceCursorV1, ObservationSourceIdentityV1,
    SourceAcquisitionCapabilitiesV1, SourceAcquisitionContractV1, SourceBindingOwnerV1,
    SourceBindingV1, SourceCaptureModeV1, SourceDefinitionV1, SourceDeletionSemanticsV1,
    SourceInstanceId, SourceRefetchStrategyV1, UtcMicros, canonical_sha256,
};
use tracedecay_store::{
    ExternalSourceReadOperationV1, ExternalSourceReadResultV1, RepositoryReadOperationV1,
    RepositoryReadResultV1, RuntimeReadCoverageV1, RuntimeReadOperationV1, RuntimeRequestProbeV1,
    StorageRuntimeReadPort as _, StoreShardScopeV1,
};

use super::{
    HostAdmissionOutcome, HostAdmissionScope, HostAdmissionStatus, HostAdmissionTestRuntimeV1,
};
use crate::errors::{Result, TraceDecayError};

impl HostAdmissionTestRuntimeV1 {
    #[doc(hidden)]
    pub async fn call_user_lcm_tool_for_test(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
        profile_root: &Path,
    ) -> Result<crate::mcp::tools::ToolResult> {
        crate::mcp::tools::handle_user_lcm_tool_with_retained_authority(
            tool_name,
            arguments,
            profile_root,
            &self.profile_registered,
            None,
        )
        .await
    }

    #[doc(hidden)]
    pub async fn call_mcp_tool_for_test(
        &self,
        cg: &crate::tracedecay::TraceDecay,
        tool_name: &str,
        arguments: serde_json::Value,
        server_stats: Option<serde_json::Value>,
        scope_prefix: Option<&str>,
    ) -> Result<crate::mcp::ToolResult> {
        let project_registry_reads = crate::mcp::server::DaemonProjectRegistryReadService::new(
            Arc::clone(&self.profile_database),
        );
        let workflow_index_reads = self.project_registered.as_ref().map(|database| {
            crate::mcp::server::DaemonWorkflowIndexReadService::new(Arc::clone(database))
        });
        crate::mcp::tools::handle_tool_call_with_registry_and_implicit_project(
            cg,
            tool_name,
            arguments,
            server_stats,
            scope_prefix,
            crate::mcp::tools::ToolCallRegistryOptions {
                global_db: Some(&self.profile_database),
                project_registry_reads: Some(&project_registry_reads),
                workflow_index_reads: workflow_index_reads
                    .as_ref()
                    .map(|service| service as &dyn tracedecay_sessions::WorkflowIndexReadPort),
                accounting_db: Some(self.profile_database.as_ref()),
                registered_project_session_db: self.project_registered.clone(),
                registered_savings_db: Some(Arc::clone(&self.profile_database)),
                profile_root: Some(&self.profile_root),
                implicit_project_path: Some(cg.project_root()),
                session_authorities: self.mcp_session_authorities(),
                ..Default::default()
            },
        )
        .await
    }

    #[doc(hidden)]
    pub fn session_temporal_store(
        &self,
        scope: HostAdmissionScope,
    ) -> std::result::Result<crate::store::GlobalDbSessionTemporalStore<'_>, HostAdmissionOutcome>
    {
        self.session_temporal_store_for_test(scope).map_err(|_| {
            HostAdmissionOutcome::retained_unavailable("registered_authority_unavailable")
        })
    }

    /// Runs workflow ingestion through this runtime's exact ProjectSessions mount.
    #[doc(hidden)]
    pub async fn ingest_workflows_for_test(
        &self,
        project_root: &Path,
    ) -> Result<crate::sessions::workflow_ingest::WorkflowIngestStats> {
        let project_id = self
            .project_id
            .as_ref()
            .ok_or_else(|| TraceDecayError::Database {
                operation: "ingest workflow test fixture".to_owned(),
                message: "project session authority is unavailable".to_owned(),
            })?;
        let database = self.project_database_for_test()?;
        let Some(home) = crate::sessions::home_dir() else {
            return Ok(crate::sessions::workflow_ingest::WorkflowIngestStats::default());
        };
        let store = crate::store::GlobalDbWorkflowStore::new(database);
        Ok(
            crate::sessions::workflow_ingest::ingest_workflow_runs_with_sink(
                &store,
                project_id,
                project_root,
                &home.join(".claude").join("projects"),
            )
            .await,
        )
    }

    /// Records one git span through this runtime's retained ProjectSessions authority.
    #[doc(hidden)]
    pub async fn record_project_span_for_test(
        &self,
        observation: &crate::sessions::git_correlation::SpanObservation,
        merge_gap_secs: i64,
    ) -> Result<i64> {
        crate::store::GlobalDbGitCorrelationStore::new(self.project_database_for_test()?)
            .record_span_observation(observation, merge_gap_secs)
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "record workflow test git span".to_owned(),
                message: error.to_string(),
            })
    }

    #[doc(hidden)]
    pub async fn record_session_span_for_test(
        &self,
        scope: HostAdmissionScope,
        observation: &crate::sessions::git_correlation::SpanObservation,
        merge_gap_secs: i64,
    ) -> Result<i64> {
        crate::store::GlobalDbGitCorrelationStore::new(self.session_database_for_test(scope)?)
            .record_span_observation(observation, merge_gap_secs)
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "record registered session span".to_owned(),
                message: error.to_string(),
            })
    }

    #[doc(hidden)]
    pub async fn drop_project_workflow_schema_for_test(&self) -> Result<()> {
        self.project_database_for_test()?
            .writer_connection()?
            .execute_batch(
                "DROP TABLE workflow_agents;
                 DROP TABLE workflow_runs;",
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "drop registered project workflow schema fixture".to_owned(),
                message: error.to_string(),
            })
    }

    /// Bind the registered workflow/handoff authority for the mounted project.
    #[doc(hidden)]
    pub fn project_workflow_storage_for_test(
        &self,
    ) -> Result<tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority> {
        self.project_database_for_test()?.workflow_storage()
    }

    #[doc(hidden)]
    pub async fn project_git_sessions_for_test(
        &self,
        query: &crate::sessions::git_correlation::SessionsForQuery,
    ) -> Result<Vec<crate::sessions::git_correlation::SessionGitCorrelationHit>> {
        crate::store::GlobalDbGitCorrelationStore::new(self.project_database_for_test()?)
            .sessions_for_with_relation(
                query,
                crate::sessions::git_correlation::CommitRelationFilter::Produced,
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "query registered project git sessions".to_owned(),
                message: error.to_string(),
            })
    }

    #[doc(hidden)]
    pub async fn run_git_backfill_for_test(
        &self,
        analytics_events: &[crate::global_db::AnalyticsEventRecord],
        git: &dyn crate::sessions::git_correlation::GitReflogSource,
        options: &crate::sessions::git_correlation::BackfillOptions,
    ) -> std::result::Result<
        crate::sessions::git_correlation::BackfillStats,
        crate::sessions::git_correlation::GitCorrelationError,
    > {
        let database = self.project_database_for_test().map_err(|error| {
            crate::sessions::git_correlation::GitCorrelationError::Db(error.to_string())
        })?;
        crate::store::GlobalDbGitCorrelationStore::new(database)
            .run_backfill(analytics_events, git, options)
            .await
    }

    #[doc(hidden)]
    pub async fn run_incremental_git_backfill_for_test(
        &self,
        git: &dyn crate::sessions::git_correlation::GitReflogSource,
        limit_sessions: usize,
    ) -> std::result::Result<
        crate::sessions::git_correlation::BackfillStats,
        crate::sessions::git_correlation::GitCorrelationError,
    > {
        let database = self.project_database_for_test().map_err(|error| {
            crate::sessions::git_correlation::GitCorrelationError::Db(error.to_string())
        })?;
        crate::store::GlobalDbGitCorrelationStore::new(database)
            .run_incremental_backfill(git, limit_sessions)
            .await
    }

    #[doc(hidden)]
    pub async fn git_correlation_meta_for_test(
        &self,
        key: &str,
    ) -> std::result::Result<Option<i64>, crate::sessions::git_correlation::GitCorrelationError>
    {
        let database = self.project_database_for_test().map_err(|error| {
            crate::sessions::git_correlation::GitCorrelationError::Db(error.to_string())
        })?;
        let snapshot = crate::store::GlobalDbGitCorrelationStore::new(database)
            .read_snapshot()
            .await?;
        crate::sessions::git_correlation::read_meta_value(&snapshot, key).await
    }

    #[doc(hidden)]
    pub async fn project_workflow_fact_rows_for_test(
        &self,
    ) -> Result<Vec<(String, Option<String>, Option<String>)>> {
        self.project_database_for_test()?.workflow_fact_rows().await
    }

    /// Counts only the bounded durable observation tables used by restart tests.
    #[doc(hidden)]
    pub async fn project_observation_table_count_for_test(&self, table: &str) -> Result<u64> {
        if !matches!(
            table,
            "observations"
                | "sanitization_receipts"
                | "projection_queue"
                | "observation_workflow_facts"
        ) {
            return Err(TraceDecayError::Database {
                operation: "count registered project observation table".to_owned(),
                message: format!("unsupported test table {table}"),
            });
        }
        let snapshot = self
            .project_database_for_test()?
            .read_snapshot()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "open registered project observation count snapshot".to_owned(),
                message: error.to_string(),
            })?;
        let mut rows = snapshot
            .query(&format!("SELECT COUNT(*) FROM {table}"), ())
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "count registered project observation table".to_owned(),
                message: error.to_string(),
            })?;
        let row = rows
            .next()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "read registered project observation table count".to_owned(),
                message: error.to_string(),
            })?
            .ok_or_else(|| TraceDecayError::Database {
                operation: "read registered project observation table count".to_owned(),
                message: "count query returned no row".to_owned(),
            })?;
        row.get::<i64>(0)
            .map(|count| u64::try_from(count).unwrap_or_default())
            .map_err(|error| TraceDecayError::Database {
                operation: "decode registered project observation table count".to_owned(),
                message: error.to_string(),
            })
    }

    #[doc(hidden)]
    pub async fn project_observation_source_cursor_for_test(
        &self,
        source: &ObservationSourceIdentityV1,
    ) -> std::result::Result<Option<ObservationSourceCursorV1>, HostAdmissionOutcome> {
        let project_id = self.project_id.as_ref().ok_or(HostAdmissionOutcome {
            status: HostAdmissionStatus::Unavailable,
            retryable: false,
            reason_code: Some("project_authority_unbound"),
        })?;
        self.facade()
            .get_source_cursor(
                source,
                &ObservationScopeV1::Project {
                    project_id: project_id.clone(),
                },
            )
            .await
    }

    #[doc(hidden)]
    pub async fn external_source_receipt_for_test(
        &self,
        scope: HostAdmissionScope,
        observation: &tracedecay_store::ObservationCommitReceipt,
    ) -> std::result::Result<Option<tracedecay_store::SourceCommitReceiptV1>, HostAdmissionOutcome>
    {
        let database = self.registered_database(scope).ok_or_else(|| {
            HostAdmissionOutcome::retained_unavailable("registered_authority_unavailable")
        })?;
        let binding =
            host_observation_source_binding_for_test(observation, database.runtime().binding())?;
        let binding_identity = binding
            .immutable_identity()
            .map_err(external_source_read_failed)?;
        let idempotency_key = crate::request_identity::derive_logical_effect_idempotency(
            crate::request_identity::LogicalEffectIdempotencyDomain::HostObservation,
            observation.observation().observation_id(),
        )
        .map_err(external_source_read_failed)?;
        let request = external_source_runtime_read_request(
            database.runtime().binding(),
            ExternalSourceReadOperationV1::State {
                binding: binding_identity,
            },
        )?;
        let probe = ExternalSourceRuntimeReadProbe::from_control(request.control());
        let outcome = database
            .runtime()
            .read(request, &probe)
            .await
            .map_err(external_source_read_failed)?;
        if !matches!(
            outcome.coverage(),
            RuntimeReadCoverageV1::Latest { .. } | RuntimeReadCoverageV1::Complete { .. }
        ) {
            return Err(external_source_read_failed(
                "external source read coverage is incomplete",
            ));
        }
        match outcome.value() {
            Some(tracedecay_store::RuntimeReadResultV1::Repository {
                result:
                    RepositoryReadResultV1::ExternalSource(ExternalSourceReadResultV1::State(Some(
                        state,
                    ))),
            }) => Ok(state.receipt_by_idempotency_key(&idempotency_key).cloned()),
            Some(tracedecay_store::RuntimeReadResultV1::Repository {
                result:
                    RepositoryReadResultV1::ExternalSource(ExternalSourceReadResultV1::State(None)),
            }) => Ok(None),
            _ => Err(external_source_read_failed(
                "external source read returned an unexpected result",
            )),
        }
    }
}

fn host_observation_source_binding_for_test(
    receipt: &tracedecay_store::ObservationCommitReceipt,
    runtime: &tracedecay_store::StoreRuntimeBindingV1,
) -> std::result::Result<SourceBindingV1, HostAdmissionOutcome> {
    let observation = receipt.observation();
    let provider = observation.source().provider().clone();
    let capabilities = SourceAcquisitionCapabilitiesV1::new(
        [SourceCaptureModeV1::Poll].into_iter().collect(),
        [SourceRefetchStrategyV1::WholeRoot].into_iter().collect(),
        [SourceDeletionSemanticsV1::ExplicitOnly]
            .into_iter()
            .collect(),
    )
    .map_err(external_source_read_failed)?;
    let definition = SourceDefinitionV1::new(
        SourceInstanceId::new(format!("source.host-observation.{}", provider.as_str()))
            .map_err(external_source_read_failed)?,
        1,
        SourceAcquisitionContractV1::new(provider, capabilities)
            .map_err(external_source_read_failed)?,
        SourceCaptureModeV1::Poll,
        SourceRefetchStrategyV1::WholeRoot,
        SourceDeletionSemanticsV1::ExplicitOnly,
        1,
    )
    .map_err(external_source_read_failed)?;
    let owner = match observation.scope() {
        ObservationScopeV1::Project { project_id } => {
            SourceBindingOwnerV1::Project(project_id.clone())
        }
        ObservationScopeV1::Profile => {
            SourceBindingOwnerV1::Profile(runtime.shard_id.profile_id.clone())
        }
    };
    let native_root = LocatorDigest::new(
        canonical_sha256(&(
            "tracedecay.host-observation.native-root.v1",
            observation.source(),
            observation.scope(),
        ))
        .map_err(external_source_read_failed)?
        .as_str(),
    )
    .map_err(external_source_read_failed)?;
    let binding = SourceBindingV1::new(
        &definition,
        owner,
        receipt
            .retrieval_anchor()
            .authorization()
            .privacy_domain_id
            .clone(),
        native_root,
        1,
    )
    .map_err(external_source_read_failed)?;
    let exact_scope = match (&binding.owner, &runtime.shard_id.scope) {
        (
            SourceBindingOwnerV1::Project(project_id),
            StoreShardScopeV1::Project {
                project_id: shard_project,
            }
            | StoreShardScopeV1::ProjectSessions {
                project_id: shard_project,
            },
        ) => project_id == shard_project,
        (
            SourceBindingOwnerV1::Profile(profile_id),
            StoreShardScopeV1::Profile | StoreShardScopeV1::ProfileSessions,
        ) => profile_id == &runtime.shard_id.profile_id,
        _ => false,
    };
    if !exact_scope {
        return Err(external_source_read_failed(
            "host observation source authority does not match the selected store shard",
        ));
    }
    Ok(binding)
}

fn external_source_runtime_read_request(
    binding: &tracedecay_store::StoreRuntimeBindingV1,
    operation: ExternalSourceReadOperationV1,
) -> std::result::Result<tracedecay_store::RuntimeReadRequestV1, HostAdmissionOutcome> {
    let digest = canonical_sha256(&operation).map_err(external_source_read_failed)?;
    let suffix = digest_suffix(digest.as_str())?;
    let requested_at = runtime_now();
    tracedecay_store::RuntimeReadRequestV1::new(
        binding.clone(),
        tracedecay_store::ConsistencyModeV1::LatestAvailable,
        RuntimeReadOperationV1::Repository {
            op: RepositoryReadOperationV1::ExternalSource(operation),
        },
        tracedecay_store::OperationPriorityV1::Foreground,
        1,
        tracedecay_store::RuntimeRequestControlV1 {
            requested_at,
            deadline: tracedecay_store::RuntimeDeadlineV1 {
                deadline_id: tracedecay_store::RuntimeDeadlineIdV1::new(format!(
                    "deadline.external-source.{suffix}"
                ))
                .map_err(external_source_read_failed)?,
            },
            cancellation: tracedecay_store::RuntimeCancellationIdentityV1 {
                cancellation_id: tracedecay_store::RuntimeCancellationIdV1::new(format!(
                    "cancellation.external-source.{suffix}"
                ))
                .map_err(external_source_read_failed)?,
                generation: 1,
            },
        },
    )
    .map_err(external_source_read_failed)
}

struct ExternalSourceRuntimeReadProbe {
    cancellation: tracedecay_store::RuntimeCancellationIdentityV1,
    deadline: tracedecay_store::RuntimeDeadlineV1,
}

impl ExternalSourceRuntimeReadProbe {
    fn from_control(control: &tracedecay_store::RuntimeRequestControlV1) -> Self {
        Self {
            cancellation: control.cancellation.clone(),
            deadline: control.deadline.clone(),
        }
    }
}

impl RuntimeRequestProbeV1 for ExternalSourceRuntimeReadProbe {
    fn cancellation_identity(&self) -> &tracedecay_store::RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &tracedecay_store::RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<tracedecay_store::RuntimeInterruptionV1> {
        None
    }
}

fn digest_suffix(digest: &str) -> std::result::Result<&str, HostAdmissionOutcome> {
    digest
        .strip_prefix("sha256:")
        .ok_or_else(|| external_source_read_failed("non-canonical external-source digest"))
}

fn runtime_now() -> UtcMicros {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    UtcMicros(i64::try_from(micros).unwrap_or(i64::MAX))
}

fn external_source_read_failed(_error: impl std::fmt::Display) -> HostAdmissionOutcome {
    HostAdmissionOutcome::retained_unavailable("external_source_read_failed")
}

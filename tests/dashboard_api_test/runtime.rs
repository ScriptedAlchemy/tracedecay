use std::collections::BTreeSet;
use std::fmt::Display;
use std::path::Path;
use std::sync::Arc;

use tracedecay::dashboard;
use tracedecay::errors::{Result, TraceDecayError};
use tracedecay::tracedecay::{TraceDecay, TraceDecayOpenOptions};
use tracedecay_application::{
    CancellationSignal, CapabilityGrantId, CapabilityGrantSnapshot, DisclosureClass,
    RequestAdmission, RequestContext, ResolvedScope,
};
use tracedecay_code_index::graph_projection::{
    CodeGraphProjectionStore, HermeticCodeGraphProjectionStore,
};
use tracedecay_code_index::lineage::GenerationSymbolIndexV1;
use tracedecay_domain::{ActorId, CodeGenerationId, ManifestDigest, ProjectId};
use tracedecay_global_db::{RegisteredGlobalDb, RegisteredGlobalDbLeaseV1};
use tracedecay_graph_db::NeverCancelled;
use tracedecay_sessions::runtime::{SessionMessageRecord, SessionRecord};
use tracedecay_usecases::context::RegisteredScopeResolver;
use tracedecay_usecases::graph::{
    CodeGraphProjectionReadPort, CodeGraphReadAdmissionFuture, CodeGraphReadAdmissionPort,
    CodeGraphReadAdmissionRequest, CodeGraphReadError, CodeGraphReadFuture, CodeGraphReadRequest,
    VerifiedCodeGraphRead,
};
use tracedecay_usecases::host_admission::HostAdmissionScope;

#[derive(Clone)]
struct DashboardTestCodeGraphProjectionV1 {
    scope: ResolvedScope,
    store: Arc<CodeGraphProjectionStore>,
}

impl CodeGraphProjectionReadPort for DashboardTestCodeGraphProjectionV1 {
    fn open<'a>(&'a self, request: CodeGraphReadRequest<'a>) -> CodeGraphReadFuture<'a> {
        Box::pin(async move {
            request
                .context
                .validate()
                .map_err(|error| CodeGraphReadError::InvalidRequest {
                    detail: error.to_string(),
                })?;
            if request.context.scope() != &self.scope {
                return Err(CodeGraphReadError::Denied);
            }
            if request.cancellation.is_cancelled() {
                return Err(CodeGraphReadError::Cancelled);
            }
            match request.context.admission_at(request.observed_at) {
                RequestAdmission::Admitted => {
                    VerifiedCodeGraphRead::new(self.scope.clone(), Arc::clone(&self.store))
                }
                RequestAdmission::Cancelled => Err(CodeGraphReadError::Cancelled),
                RequestAdmission::TimedOut => Err(CodeGraphReadError::TimedOut),
            }
        })
    }
}

#[derive(Clone)]
struct DashboardTestCodeGraphAdmissionV1 {
    scope: ResolvedScope,
}

impl CodeGraphReadAdmissionPort for DashboardTestCodeGraphAdmissionV1 {
    fn admit<'a>(
        &'a self,
        request: CodeGraphReadAdmissionRequest<'a>,
    ) -> CodeGraphReadAdmissionFuture<'a> {
        Box::pin(async move {
            if request.cancellation.is_cancelled() {
                return Err(CodeGraphReadError::Cancelled);
            }
            if request.deadline.is_elapsed_at(request.observed_at) {
                return Err(CodeGraphReadError::TimedOut);
            }
            let actor = ActorId::new("actor.dashboard-test-code-graph").map_err(|error| {
                CodeGraphReadError::InvalidRequest {
                    detail: error.to_string(),
                }
            })?;
            let grant = CapabilityGrantSnapshot::new(
                CapabilityGrantId::new("grant.dashboard-test-code-graph").map_err(|error| {
                    CodeGraphReadError::InvalidRequest {
                        detail: error.to_string(),
                    }
                })?,
                1,
                ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).map_err(|error| {
                    CodeGraphReadError::InvalidRequest {
                        detail: error.to_string(),
                    }
                })?,
                actor.clone(),
                request.observed_at,
                request.deadline.expires_at,
                self.scope.clone(),
                BTreeSet::from([request.operation.capability_id().clone()]),
                BTreeSet::from([request.operation.use_case_id().clone()]),
                DisclosureClass::Evidence,
            )
            .map_err(|error| CodeGraphReadError::InvalidRequest {
                detail: error.to_string(),
            })?;
            RequestContext::new(
                actor,
                self.scope.clone(),
                grant,
                request.request_id,
                request.deadline,
                request.cancellation.context(),
            )
            .map_err(|error| CodeGraphReadError::InvalidRequest {
                detail: error.to_string(),
            })
        })
    }
}

/// Dashboard integration authority assembled at the root composition layer.
///
/// This wrapper owns the production registry-backed storage, graph, and HTTP
/// authority wiring needed by root tests.
pub(crate) struct DashboardTestRuntimeV1 {
    profile_root: std::path::PathBuf,
    profile_database: RegisteredGlobalDbLeaseV1,
    project_database: RegisteredGlobalDbLeaseV1,
    graph: dashboard::DashboardGraphTestRuntimeV1,
    project_id: ProjectId,
}

impl DashboardTestRuntimeV1 {
    pub(crate) async fn project(
        profile_root: impl AsRef<Path>,
        project_root: impl AsRef<Path>,
        project_id: ProjectId,
    ) -> Result<Self> {
        dashboard::register_test_schema_installer();
        let profile_root = profile_root.as_ref();
        let project_root = project_root.as_ref();
        std::fs::create_dir_all(project_root)?;
        // Production projects gain a repository identity marker at
        // registration (`lifecycle::registry`); the dashboard resolves its
        // exact application scope ONCE from that marker when the server state
        // is built. A fixture without it would serve every scope-bound read
        // (delivery, explorer, storage telemetry, provider usage) as typed
        // unavailable, which is not the journey these tests prove.
        initialize_fixture_repository_identity(project_root, &project_id)?;

        let graph_profile_root = profile_root
            .join("dashboard-test-graphs")
            .join(project_id.as_str());
        let graph = dashboard::DashboardGraphTestRuntimeV1::open(&graph_profile_root).await?;
        let profile_database = graph.profile_database();
        let project_database = graph
            .project_sessions(project_root, project_id.clone())
            .await?;
        // Graph databases stay isolated under `dashboard-test-graphs`, but the
        // profile root handed to the automation/skills authority must be the
        // real resolved user profile root: production gates managed-skill
        // exports on `uses_default_user_profile`, and the outcomes/skills
        // endpoints read the same root fixtures write through
        // `default_profile_root()`.
        Ok(Self {
            profile_root: profile_root.to_path_buf(),
            profile_database,
            project_database,
            graph,
            project_id,
        })
    }

    pub(crate) fn canonical_project_key(project_path: &Path) -> String {
        RegisteredGlobalDb::canonical_project_key(project_path)
    }

    pub(crate) fn profile_root(&self) -> &Path {
        &self.profile_root
    }

    pub(crate) async fn initialize_project_graph_for_test(
        &self,
        project_root: &Path,
        _open_options: TraceDecayOpenOptions,
    ) -> Result<TraceDecay> {
        self.initialize_project_graph_with_id_for_test(project_root, self.project_id.clone())
            .await
    }

    pub(crate) async fn initialize_project_graph_with_id_for_test(
        &self,
        project_root: &Path,
        project_id: ProjectId,
    ) -> Result<TraceDecay> {
        let graph = self
            .graph
            .initialize(project_root, project_id.clone())
            .await?;
        self.profile_database
            // Propagated: the registry's own typed refusal/conflict/database
            // states name why the fixture root was not admitted, which the
            // former blanket "was rejected by the registry" message erased.
            .upsert_code_project(project_id.as_str(), project_root, None, None, None)
            .await?;
        Ok(graph)
    }

    pub(crate) async fn open_project_graph_for_test(
        &self,
        project_root: &Path,
        _open_options: TraceDecayOpenOptions,
    ) -> Result<TraceDecay> {
        self.graph.reopen(project_root).await
    }

    pub(crate) fn dashboard_test_authority(
        self: &Arc<Self>,
    ) -> Result<dashboard::DashboardHostAdmissionTestAuthorityV1> {
        Ok(dashboard::DashboardHostAdmissionTestAuthorityV1::new(
            Arc::clone(self),
            self.profile_database.clone(),
            self.project_database.clone(),
        ))
    }

    /// The dashboard authority plus the daemon-owned LCM and verified graph
    /// read ports — the composition production mounts for `hermes-lcm`,
    /// explorer, and `/api/plugins/graph/*` reads.
    pub(crate) async fn dashboard_test_authority_with_session_reads(
        self: &Arc<Self>,
        cg: &Arc<TraceDecay>,
    ) -> Result<dashboard::DashboardHostAdmissionTestAuthorityV1> {
        let authority = self.dashboard_test_authority()?;
        let (automation_authority, automation_writer) =
            dashboard::dashboard_automation_authority_for_test(Arc::clone(cg), &self.profile_root)
                .await?;
        let lcm_read_authority = dashboard::dashboard_lcm_read_authority_for_test(
            cg.as_ref(),
            self.profile_database.as_ref(),
            self.project_database.clone(),
        )
        .await
        .ok_or_else(|| TraceDecayError::Config {
            message: "dashboard fixture could not compose the LCM read authority".to_owned(),
        })?;
        let git_correlation_read_authority =
            dashboard::dashboard_git_correlation_read_authority_for_test(
                self.project_database.clone(),
            );
        let (code_graph_admission, code_graph_projection) =
            dashboard_test_code_graph_authority(cg.as_ref(), &self.project_id)?;
        let authority = authority
            .with_automation_authority(automation_authority, automation_writer)
            .with_lcm_read_authority(lcm_read_authority)
            .with_git_correlation_read_authority(git_correlation_read_authority)
            .with_code_graph_authority(code_graph_admission, code_graph_projection);
        Ok(authority)
    }

    /// Adds the real daemon configuration mutation service for endpoint tests
    /// that exercise revision-fenced dashboard writes.
    pub(crate) async fn dashboard_test_authority_with_configuration(
        self: &Arc<Self>,
        cg: &Arc<TraceDecay>,
    ) -> Result<dashboard::DashboardHostAdmissionTestAuthorityV1> {
        let authority = self.dashboard_test_authority_with_session_reads(cg).await?;
        let application_runtime =
            dashboard::dashboard_configuration_application_runtime_for_test(Arc::clone(cg)).await?;
        Ok(authority.with_application_invocation_executor(application_runtime))
    }

    /// The registered project identity this fixture runtime was opened for.
    pub(crate) fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    /// Seeds one canonical message observation through the production
    /// observation-capture route; the temporal projection discovers sessions
    /// only from these effects, never from raw session-message upserts.
    pub(crate) async fn seed_session_message_observation_for_test(
        &self,
        seed: dashboard::observation_seed::DashboardSessionMessageSeedV1<'_>,
    ) -> Result<()> {
        dashboard::observation_seed::seed_session_message_observation_for_test(
            self.project_database.as_ref(),
            seed,
        )
        .await
    }

    /// Materializes the pending session-temporal refresh for one seeded
    /// session so daemon LCM/explorer reads serve it.
    pub(crate) async fn materialize_session_temporal_refresh_for_test(
        &self,
        session_id: &str,
    ) -> Result<()> {
        dashboard::observation_seed::materialize_session_temporal_refresh_for_test(
            self.project_database.as_ref(),
            session_id,
        )
        .await
    }

    fn database(&self, scope: HostAdmissionScope) -> Result<&RegisteredGlobalDb> {
        match scope {
            HostAdmissionScope::Project => Ok(self.project_database.as_ref()),
            HostAdmissionScope::Profile => Ok(self.profile_database.as_ref()),
        }
    }

    pub(crate) fn database_path(&self, scope: HostAdmissionScope) -> Option<&Path> {
        self.database(scope).ok().map(RegisteredGlobalDb::db_path)
    }

    pub(crate) async fn upsert_code_project(
        &self,
        project_id: &str,
        project_root: &Path,
        git_common_dir: Option<&Path>,
        git_remote_url: Option<&str>,
        default_branch: Option<&str>,
    ) -> Result<tracedecay_global_db::CodeProjectRecord> {
        self.profile_database
            .upsert_code_project(
                project_id,
                project_root,
                git_common_dir,
                git_remote_url,
                default_branch,
            )
            .await
    }

    pub(crate) async fn append_analytics_event_for_test(
        &self,
        scope: HostAdmissionScope,
        event: &tracedecay_global_db::AnalyticsEventInsert,
    ) -> Result<i64> {
        self.database(scope)?
            .append_analytics_event(event)
            .await
            .map_err(|message| TraceDecayError::Database {
                operation: "append dashboard test analytics event".to_owned(),
                message,
            })
    }

    pub(crate) async fn append_analytics_events_for_test(
        &self,
        scope: HostAdmissionScope,
        events: &[tracedecay_global_db::AnalyticsEventInsert],
    ) -> Result<Vec<i64>> {
        self.database(scope)?
            .append_analytics_events(events)
            .await
            .map_err(|message| TraceDecayError::Database {
                operation: "append dashboard test analytics event batch".to_owned(),
                message,
            })
    }

    pub(crate) async fn record_savings_for_test(
        &self,
        project: &str,
        tool: &str,
        before: u64,
        after: u64,
        timestamp: i64,
    ) {
        self.profile_database
            .record_savings(project, tool, before, after, timestamp)
            .await;
    }

    pub(crate) async fn upsert(&self, project_path: &Path, tokens_saved: u64) {
        self.profile_database
            .upsert(project_path, tokens_saved)
            .await;
    }

    pub(crate) async fn upsert_session_for_test(
        &self,
        scope: HostAdmissionScope,
        session: &SessionRecord,
    ) -> Result<bool> {
        Ok(self.database(scope)?.upsert_session(session).await)
    }

    pub(crate) async fn upsert_session_message_for_test(
        &self,
        scope: HostAdmissionScope,
        message: &SessionMessageRecord,
    ) -> Result<bool> {
        let database = self.database(scope)?;
        let session = database
            .get_session(&message.provider, &message.session_id)
            .await
            .ok_or_else(|| TraceDecayError::Database {
                operation: "seed dashboard test session message".to_owned(),
                message: format!(
                    "session {}/{} is unavailable",
                    message.provider, message.session_id
                ),
            })?;
        Ok(database
            .upsert_transcript_batch(
                &session,
                std::slice::from_ref(message),
                &format!(
                    "dashboard-test-message:{}:{}",
                    message.provider, message.message_id
                ),
                tracedecay_global_db::ParseOffset::default(),
            )
            .await)
    }

    pub(crate) async fn upsert_transcript_batch_for_test(
        &self,
        scope: HostAdmissionScope,
        session: &SessionRecord,
        messages: &[SessionMessageRecord],
        source: &str,
        offset: tracedecay_global_db::ParseOffset,
    ) -> Result<Vec<i64>> {
        let database = self.database(scope)?;
        if !database
            .upsert_transcript_batch(session, messages, source, offset)
            .await
        {
            return Err(TraceDecayError::Database {
                operation: "seed dashboard test transcript batch".to_owned(),
                message: "registered transcript batch write failed".to_owned(),
            });
        }
        let mut store_ids = Vec::with_capacity(messages.len());
        for message in messages {
            let raw = load_registered_raw_message(database, &message.provider, &message.message_id)
                .await
                .ok_or_else(|| TraceDecayError::Database {
                    operation: "read dashboard test transcript store id".to_owned(),
                    message: format!(
                        "LCM raw message {}/{} is unavailable after insert",
                        message.provider, message.message_id
                    ),
                })?;
            store_ids.push(raw.store_id);
        }
        Ok(store_ids)
    }

    pub(crate) async fn session_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: &str,
        session_id: &str,
    ) -> Result<Option<SessionRecord>> {
        Ok(self
            .database(scope)?
            .get_session(provider, session_id)
            .await)
    }

    pub(crate) async fn record_project_span_for_test(
        &self,
        observation: &tracedecay_sessions::runtime::git_correlation::SpanObservation,
        merge_gap_secs: i64,
    ) -> Result<i64> {
        dashboard::record_project_span_for_test(
            self.project_database.as_ref(),
            observation,
            merge_gap_secs,
        )
        .await
    }

    pub(crate) async fn lcm_ingest_raw_message_for_test(
        &self,
        scope: HostAdmissionScope,
        message: &SessionMessageRecord,
    ) -> std::result::Result<(), tracedecay_sessions::runtime::lcm::LcmError> {
        let database = self
            .database(scope)
            .map_err(|error| tracedecay_sessions::runtime::lcm::LcmError::Db(error.to_string()))?;
        let storage_root = database.db_path().parent().ok_or_else(|| {
            tracedecay_sessions::runtime::lcm::LcmError::Db(
                "registered session database has no storage root".to_owned(),
            )
        })?;
        database.lcm_ingest_raw_message(storage_root, message).await
    }

    pub(crate) async fn lcm_raw_store_id_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: &str,
        message_id: &str,
    ) -> Result<Option<i64>> {
        let snapshot = self.database(scope)?.read_snapshot().await?;
        let mut rows = snapshot
            .query(
                "SELECT store_id FROM lcm_raw_messages
                 WHERE provider = ?1 AND message_id = ?2",
                tracedecay_runtime_core::db::engine::params![provider, message_id],
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "query dashboard test LCM store id".to_owned(),
                message: error.to_string(),
            })?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "read dashboard test LCM store id".to_owned(),
                message: error.to_string(),
            })?
        else {
            return Ok(None);
        };
        row.get::<i64>(0)
            .map(Some)
            .map_err(|error| TraceDecayError::Database {
                operation: "decode dashboard test LCM store id".to_owned(),
                message: error.to_string(),
            })
    }

    pub(crate) async fn lcm_insert_summary_node_for_test(
        &self,
        scope: HostAdmissionScope,
        draft: tracedecay_sessions::runtime::lcm::LcmSummaryNodeDraft,
    ) -> std::result::Result<
        tracedecay_sessions::runtime::lcm::LcmSummaryNode,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        let database = self
            .database(scope)
            .map_err(|error| tracedecay_sessions::runtime::lcm::LcmError::Db(error.to_string()))?;
        let summary_hash =
            tracedecay_sessions::retrieval_content::projected_content_hash(&draft.summary_text);
        let summary_id = tracedecay_sessions::runtime::lcm::dag::summary_node_id(
            &draft.provider,
            &draft.session_id,
            draft.depth,
            &draft.source_refs,
            &summary_hash,
        );
        let control = tracedecay_temporal_query::ports::ExecutionControl::default();
        database
            .lcm_publish_immutable_summary_guarded(
                tracedecay_sessions::runtime::lcm::types::LcmImmutableSummaryPublication {
                    summary_id,
                    predecessor_summary_id: None,
                    draft,
                },
                &control,
                || Ok(()),
            )
            .await
            .map(|receipt| receipt.summary)
    }
}

fn dashboard_test_code_graph_authority(
    cg: &TraceDecay,
    project_id: &ProjectId,
) -> Result<(
    Arc<dyn CodeGraphReadAdmissionPort>,
    Arc<dyn CodeGraphProjectionReadPort>,
)> {
    let scope = RegisteredScopeResolver::resolve(cg.project_root(), cg.project_root(), project_id)
        .map_err(|error| dashboard_test_graph_error("resolve exact project scope", error))?;
    let generation = CodeGenerationId::new("generation.dashboard-test-code-graph.1")
        .map_err(|error| dashboard_test_graph_error("create generation identity", error))?;
    let cancellation = CancellationSignal::active("cancel.dashboard-test-code-graph")
        .map_err(|error| dashboard_test_graph_error("create cancellation authority", error))?;
    let projection = HermeticCodeGraphProjectionStore::memory(&cancellation)
        .map_err(|error| dashboard_test_graph_error("create projection store", error))?;
    let symbols = GenerationSymbolIndexV1::new(generation.clone(), Vec::new())
        .map_err(|error| dashboard_test_graph_error("create generation symbol index", error))?;
    projection
        .publish_indexed_with_cancellation(
            &generation,
            &[],
            &[],
            &[],
            &symbols,
            Arc::new(NeverCancelled),
        )
        .map_err(|error| dashboard_test_graph_error("publish verified generation", error))?;
    let store = Arc::new(
        projection
            .verified_store(&generation)
            .map_err(|error| dashboard_test_graph_error("open verified generation", error))?,
    );
    Ok((
        Arc::new(DashboardTestCodeGraphAdmissionV1 {
            scope: scope.clone(),
        }),
        Arc::new(DashboardTestCodeGraphProjectionV1 { scope, store }),
    ))
}

fn dashboard_test_graph_error(operation: &str, error: impl Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("dashboard fixture could not {operation}: {error}"),
    }
}

async fn load_registered_raw_message(
    database: &RegisteredGlobalDb,
    provider: &str,
    message_id: &str,
) -> Option<tracedecay_sessions::runtime::lcm::LcmRawMessage> {
    let snapshot = database
        .read_snapshot()
        .await
        .expect("dashboard test raw-message snapshot must remain registered");
    tracedecay_sessions::runtime::lcm::schema::load_raw_message(&snapshot, provider, message_id)
        .await
        .expect("dashboard test raw-message load must not hide database or receipt failure")
}

/// Gives the fixture root the same registered-repository identity a
/// production project gains at registration: a real git repository whose
/// common directory carries the authoritative repository identity marker.
/// The dashboard's exact scope resolution (and every read bound to it)
/// requires both; without them the fixture would only ever exercise the
/// typed-unavailable paths.
fn initialize_fixture_repository_identity(
    project_root: &Path,
    project_id: &ProjectId,
) -> Result<()> {
    if !project_root.join(".git").exists() {
        let output = std::process::Command::new(crate::common::git_program())
            .args(["init", "-b", "main"])
            .current_dir(project_root)
            .output()
            .map_err(|error| TraceDecayError::Config {
                message: format!("dashboard fixture git init failed to spawn: {error}"),
            })?;
        if !output.status.success() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "dashboard fixture git init failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                ),
            });
        }
    }
    if !tracedecay::storage::write_repository_identity_marker(project_root, project_id.as_str())? {
        return Err(TraceDecayError::Config {
            message: format!(
                "dashboard fixture repository identity marker was not written for '{}'",
                project_root.display()
            ),
        });
    }
    Ok(())
}

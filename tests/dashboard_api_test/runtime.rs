use std::path::Path;
use std::sync::Arc;

use serde_json::Value;
use tracedecay::application::host_admission::HostAdmissionScope;
use tracedecay::dashboard;
use tracedecay::errors::{Result, TraceDecayError};
use tracedecay::sessions::{SessionMessageRecord, SessionRecord};
use tracedecay::tracedecay::{TraceDecay, TraceDecayOpenOptions};
use tracedecay_domain::ProjectId;
use tracedecay_global_db::RegisteredGlobalDb;

/// Dashboard integration authority assembled at the root composition layer.
///
/// This wrapper owns the production registry-backed storage, graph, and HTTP
/// authority wiring needed by root tests.
pub(crate) struct DashboardTestRuntimeV1 {
    profile_database: Arc<RegisteredGlobalDb>,
    project_database: Arc<RegisteredGlobalDb>,
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
        tracedecay::storage::write_enrollment_marker(
            project_root,
            &tracedecay::storage::EnrollmentMarker {
                project_id: project_id.as_str().to_owned(),
                storage_mode: tracedecay::storage::StorageMode::ProfileSharded,
            },
        )?;

        let graph_profile_root = profile_root
            .join("dashboard-test-graphs")
            .join(project_id.as_str());
        let graph = dashboard::DashboardGraphTestRuntimeV1::open(graph_profile_root).await?;
        let profile_database = graph.profile_database();
        let project_database = graph
            .project_sessions(project_root, project_id.clone())
            .await?;
        Ok(Self {
            profile_database,
            project_database,
            graph,
            project_id,
        })
    }

    pub(crate) fn canonical_project_key(project_path: &Path) -> String {
        RegisteredGlobalDb::canonical_project_key(project_path)
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
            Arc::clone(&self.profile_database),
            Arc::clone(&self.project_database),
        ))
    }

    /// The dashboard authority plus the daemon-owned LCM and verified graph
    /// read ports — the composition production mounts for `hermes-lcm`,
    /// explorer, and `/api/plugins/graph/*` reads.
    pub(crate) async fn dashboard_test_authority_with_session_reads(
        self: &Arc<Self>,
        cg: &TraceDecay,
    ) -> Result<dashboard::DashboardHostAdmissionTestAuthorityV1> {
        let authority = self.dashboard_test_authority()?;
        let lcm_read_authority = dashboard::dashboard_lcm_read_authority_for_test(
            cg,
            self.profile_database.as_ref(),
            Arc::clone(&self.project_database),
        )
        .await
        .ok_or_else(|| TraceDecayError::Config {
            message: "dashboard fixture could not compose the LCM read authority".to_owned(),
        })?;
        let graph_read_authority =
            dashboard::dashboard_graph_read_authority_for_test(cg, self.project_database.as_ref())
                .ok_or_else(|| TraceDecayError::Config {
                    message: "dashboard fixture could not compose the graph read authority"
                        .to_owned(),
                })?;
        let git_correlation_read_authority =
            dashboard::dashboard_git_correlation_read_authority_for_test(Arc::clone(
                &self.project_database,
            ));
        Ok(authority
            .with_lcm_read_authority(lcm_read_authority)
            .with_graph_read_authority(graph_read_authority)
            .with_git_correlation_read_authority(git_correlation_read_authority))
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

    fn primary_session_database(&self) -> &RegisteredGlobalDb {
        self.project_database.as_ref()
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

    pub(crate) async fn append_profile_analytics_event_for_test(
        &self,
        event: &tracedecay_global_db::AnalyticsEventInsert,
    ) -> Result<i64> {
        self.append_analytics_event_for_test(HostAdmissionScope::Profile, event)
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
        observation: &tracedecay::sessions::git_correlation::SpanObservation,
        merge_gap_secs: i64,
    ) -> Result<i64> {
        dashboard::record_project_span_for_test(
            self.project_database.as_ref(),
            observation,
            merge_gap_secs,
        )
        .await
    }

    pub(crate) async fn lcm_load_raw_message_for_test(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Option<tracedecay::sessions::lcm::LcmRawMessage> {
        load_registered_raw_message(self.primary_session_database(), provider, message_id).await
    }

    pub(crate) async fn lcm_ingest_raw_message_for_test(
        &self,
        scope: HostAdmissionScope,
        message: &SessionMessageRecord,
    ) -> std::result::Result<(), tracedecay::sessions::lcm::LcmError> {
        let database = self
            .database(scope)
            .map_err(|error| tracedecay::sessions::lcm::LcmError::Db(error.to_string()))?;
        let storage_root = database.db_path().parent().ok_or_else(|| {
            tracedecay::sessions::lcm::LcmError::Db(
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
        draft: tracedecay::sessions::lcm::LcmSummaryNodeDraft,
    ) -> std::result::Result<
        tracedecay::sessions::lcm::LcmSummaryNode,
        tracedecay::sessions::lcm::LcmError,
    > {
        let database = self
            .database(scope)
            .map_err(|error| tracedecay::sessions::lcm::LcmError::Db(error.to_string()))?;
        let summary_hash =
            tracedecay_sessions::compatibility::projected_content_hash(&draft.summary_text);
        let summary_id = tracedecay::sessions::lcm::dag::summary_node_id(
            &draft.provider,
            &draft.session_id,
            draft.depth,
            &draft.source_refs,
            &summary_hash,
        );
        let control = tracedecay_temporal_query::ports::ExecutionControl::default();
        database
            .lcm_publish_immutable_summary_guarded(
                tracedecay::sessions::lcm::types::LcmImmutableSummaryPublication {
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

    pub(crate) async fn lcm_status_deep_for_test(
        &self,
        provider: &str,
        session_id: Option<&str>,
    ) -> std::result::Result<
        tracedecay::sessions::lcm::LcmStatus,
        tracedecay::sessions::lcm::LcmError,
    > {
        self.primary_session_database()
            .lcm_status_with_options(
                provider,
                session_id,
                true,
                &tracedecay::sessions::lcm::LcmGcConfig::default(),
            )
            .await
    }
}

async fn load_registered_raw_message(
    database: &RegisteredGlobalDb,
    provider: &str,
    message_id: &str,
) -> Option<tracedecay::sessions::lcm::LcmRawMessage> {
    let snapshot = database
        .read_snapshot()
        .await
        .expect("dashboard test raw-message snapshot must remain registered");
    tracedecay::sessions::lcm::schema::load_raw_message(&snapshot, provider, message_id)
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

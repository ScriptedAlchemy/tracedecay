use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tempfile::TempDir;

use crate::RegisteredGlobalDb;
use crate::host_ports::profile_sessions::{self, ProfileSessionsRuntime};
use tracedecay_runtime_core::db::DaemonDatabaseScope;

static TEST_RUNTIME_NONCE: AtomicU64 = AtomicU64::new(1);

/// Message shown when the composition root never installed the opener.
pub(crate) const UNWIRED_PROFILE_SESSIONS: &str = "tracedecay_global_db::host_ports::profile_sessions::register must be called by the \
     composition root before a registered harness can open";

pub struct RegisteredGlobalDbHarness {
    pub registered: Arc<RegisteredGlobalDb>,
    _directory: TempDir,
    _scope: Option<DaemonDatabaseScope>,
    registry: Box<dyn ProfileSessionsRuntime>,
}

impl RegisteredGlobalDbHarness {
    pub async fn open(label: &str) -> Self {
        let directory = tempfile::tempdir().expect("temporary registered global database");
        let profile_root = directory.path().join("profile");
        let nonce = TEST_RUNTIME_NONCE.fetch_add(1, Ordering::Relaxed);
        // The scope guard is entered before the runtime opens; the root opener
        // creates the profile identity on its way to the session registry.
        let scope =
            tracedecay_runtime_core::db::enter_daemon_database_scope(&profile_root, nonce, label)
                .expect("daemon database scope");
        let registry = profile_sessions::open(profile_root)
            .expect(UNWIRED_PROFILE_SESSIONS)
            .await;
        let registered = registry.mount().await;
        Self {
            registered,
            _directory: directory,
            _scope: Some(scope),
            registry,
        }
    }

    #[cfg(test)]
    pub(super) fn storage_root(&self) -> &std::path::Path {
        self.registered
            .db_path()
            .parent()
            .expect("registered database storage root")
    }

    pub async fn mount(&self) -> Arc<RegisteredGlobalDb> {
        self.registry.mount().await
    }

    #[cfg(test)]
    pub(super) fn revoke(&mut self) {
        drop(self._scope.take());
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostAdmissionScope {
    Project,
    Profile,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionTemporalFixtureCountV1 {
    ProjectionReceipts,
    Occurrences,
    LogicalCopyEdges,
    Assertions,
    RefreshReceipts,
    RefreshProgress,
}

/// Test-only registered database fixture retained below the use-case layer.
#[cfg(test)]
pub(crate) struct HostAdmissionTestRuntimeV1 {
    profile_registered: Arc<RegisteredGlobalDb>,
    project_registered: Option<Arc<RegisteredGlobalDb>>,
    _scope: DaemonDatabaseScope,
}

#[cfg(test)]
impl HostAdmissionTestRuntimeV1 {
    pub(crate) async fn profile(
        profile_root: impl AsRef<std::path::Path>,
    ) -> tracedecay_runtime_core::errors::Result<Self> {
        Self::open(profile_root.as_ref(), None).await
    }

    pub(crate) async fn project(
        profile_root: impl AsRef<std::path::Path>,
        project_root: impl AsRef<std::path::Path>,
        _project_id: tracedecay_domain::ProjectId,
    ) -> tracedecay_runtime_core::errors::Result<Self> {
        Self::open(profile_root.as_ref(), Some(project_root.as_ref())).await
    }

    async fn open(
        profile_root: &std::path::Path,
        project_root: Option<&std::path::Path>,
    ) -> tracedecay_runtime_core::errors::Result<Self> {
        std::fs::create_dir_all(profile_root)?;
        if let Some(project_root) = project_root {
            std::fs::create_dir_all(project_root)?;
        }
        let nonce = TEST_RUNTIME_NONCE.fetch_add(1, Ordering::Relaxed);
        let scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            profile_root,
            nonce,
            "global-db-test-runtime",
        )?;
        let profile_registered =
            open_registered_test_database(&profile_root.join("profile-sessions.db")).await?;
        let project_registered = match project_root {
            Some(project_root) => Some(
                open_registered_test_database(
                    &project_root.join(".tracedecay").join("project-sessions.db"),
                )
                .await?,
            ),
            None => None,
        };
        Ok(Self {
            profile_registered,
            project_registered,
            _scope: scope,
        })
    }

    pub(crate) fn registered_database(
        &self,
        scope: HostAdmissionScope,
    ) -> Option<&RegisteredGlobalDb> {
        match scope {
            HostAdmissionScope::Project => self.project_registered.as_deref(),
            HostAdmissionScope::Profile => Some(self.profile_registered.as_ref()),
        }
    }

    fn session_database_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::errors::Result<&RegisteredGlobalDb> {
        self.registered_database(scope).ok_or_else(|| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "bind registered global-db test runtime".to_owned(),
                message: "requested registered database scope is unavailable".to_owned(),
            }
        })
    }

    pub(crate) fn observation_store(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::errors::Result<crate::GlobalDbObservationStore<'_>> {
        let database = self.session_database_for_test(scope)?;
        Ok(crate::GlobalDbObservationStore::with_runtime(
            database.runtime(),
            database.authority(),
        ))
    }

    pub(crate) fn session_temporal_store_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::errors::Result<
        crate::session_temporal::GlobalDbSessionTemporalStore<'_>,
    > {
        Ok(crate::session_temporal::GlobalDbSessionTemporalStore::new(
            self.session_database_for_test(scope)?,
        ))
    }

    pub(crate) fn project_configuration_control_store_for_test(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<
        crate::configuration::OwnedGlobalDbConfigurationControlStore,
    > {
        let database = self.project_registered.clone().ok_or_else(|| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "bind configuration control test project sessions".to_owned(),
                message: "registered project database is unavailable".to_owned(),
            }
        })?;
        Ok(
            crate::configuration::OwnedGlobalDbConfigurationControlStore::from_registered_project_runtime_db(
                database,
            ),
        )
    }

    pub(crate) async fn session_temporal_fixture_count_for_test(
        &self,
        scope: HostAdmissionScope,
        kind: SessionTemporalFixtureCountV1,
    ) -> tracedecay_runtime_core::errors::Result<i64> {
        let table = match kind {
            SessionTemporalFixtureCountV1::ProjectionReceipts => {
                "session_temporal_projection_receipts"
            }
            SessionTemporalFixtureCountV1::Occurrences => "session_occurrences",
            SessionTemporalFixtureCountV1::LogicalCopyEdges => "session_logical_copy_edges",
            SessionTemporalFixtureCountV1::Assertions => "session_assertions",
            SessionTemporalFixtureCountV1::RefreshReceipts => "session_refresh_receipts",
            SessionTemporalFixtureCountV1::RefreshProgress => "session_refresh_progress",
        };
        let snapshot = self
            .session_database_for_test(scope)?
            .read_snapshot()
            .await?;
        let mut rows = snapshot
            .query(&format!("SELECT COUNT(*) FROM {table}"), ())
            .await?;
        let row = rows.next().await?.ok_or_else(|| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "read session-temporal fixture count".to_owned(),
                message: "count query returned no row".to_owned(),
            }
        })?;
        row.get(0).map_err(Into::into)
    }

    pub(crate) async fn session_temporal_copy_edge_for_test(
        &self,
        scope: HostAdmissionScope,
        session_id: &tracedecay_domain::SessionId,
    ) -> tracedecay_runtime_core::errors::Result<Option<(i64, tracedecay_domain::TemporalValidityV1)>>
    {
        let snapshot = self
            .session_database_for_test(scope)?
            .read_snapshot()
            .await?;
        let mut rows = snapshot
            .query(
                "SELECT knowledge_at, valid_time_json
                 FROM session_logical_copy_edges
                 WHERE session_id = ?1",
                [session_id.as_str()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let knowledge_at = row.get::<i64>(0)?;
        let valid_time_json = row.get::<String>(1)?;
        let valid_time = serde_json::from_str(&valid_time_json).map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "parse session-temporal copy edge valid time".to_owned(),
                message: error.to_string(),
            }
        })?;
        Ok(Some((knowledge_at, valid_time)))
    }

    pub(crate) async fn lcm_describe_for_test(
        &self,
        request: tracedecay_sessions::runtime::lcm::LcmDescribeRequest,
    ) -> Result<
        tracedecay_sessions::runtime::lcm::LcmDescribeResponse,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.profile_registered.lcm_describe(request).await
    }

    pub(crate) async fn lcm_expand_for_test(
        &self,
        request: tracedecay_sessions::runtime::lcm::LcmExpandRequest,
    ) -> Result<
        tracedecay_sessions::runtime::lcm::LcmExpandResponse,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.profile_registered.lcm_expand(request).await
    }

    pub(crate) async fn seed_lcm_render_fixture_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        let database = self.session_database_for_test(scope)?;
        let session = tracedecay_sessions::runtime::SessionRecord {
            provider: "codex".to_owned(),
            session_id: "session-a".to_owned(),
            project_key: "project-a".to_owned(),
            project_path: "/project-a".to_owned(),
            title: Some("Canonical render fixture".to_owned()),
            started_at: Some(10),
            ended_at: None,
            transcript_path: None,
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        };
        if !database.upsert_session(&session).await {
            return Err(tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "seed canonical lcm render session".to_owned(),
                message: "session upsert failed".to_owned(),
            });
        }

        let external_content = "canonical external payload";
        let external_hash =
            tracedecay_sessions::runtime::lcm::util::sha256_hex(external_content.as_bytes());
        let payload_dir = database
            .db_path()
            .parent()
            .ok_or_else(
                || tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "seed canonical lcm render payload".to_owned(),
                    message: "registered session database has no storage root".to_owned(),
                },
            )?
            .join("lcm-payloads");
        std::fs::create_dir_all(&payload_dir)?;
        std::fs::write(payload_dir.join("payload-a"), external_content)?;

        database
            .writer_connection()?
            .execute_batch(&format!(
                "INSERT INTO lcm_external_payloads(
                    payload_ref, provider, session_id, message_id, kind, content_hash,
                    byte_count, char_count, created_at, metadata_json
                 ) VALUES (
                    'payload-a', 'codex', 'session-a', 'message-b', 'tool_result',
                    '{external_hash}', {byte_count}, {char_count}, 12, NULL
                 );
                 INSERT INTO lcm_raw_messages(
                    provider, message_id, session_id, store_id, role, ordinal, timestamp,
                    content, content_hash, storage_kind, payload_ref, snippet_text,
                    index_text, legacy_source, legacy_truncated, metadata_json
                 ) VALUES (
                    'codex', 'message-a', 'session-a', 11, 'assistant', 0, 11,
                    'canonical raw message', 'raw-hash-a', 'inline', NULL,
                    'canonical raw message', 'canonical raw message', 0, 0, NULL
                 );
                 INSERT INTO lcm_raw_messages(
                    provider, message_id, session_id, store_id, role, ordinal, timestamp,
                    content, content_hash, storage_kind, payload_ref, snippet_text,
                    index_text, legacy_source, legacy_truncated, metadata_json
                 ) VALUES (
                    'codex', 'message-b', 'session-a', 12, 'tool', 1, 12,
                    NULL, '{external_hash}', 'external', 'payload-a',
                    'canonical external payload', 'canonical external payload', 0, 0, NULL
                 );
                 INSERT INTO lcm_summary_nodes(
                    node_id, provider, conversation_id, session_id, depth, summary_text,
                    summary_hash, summary_token_count, source_token_count,
                    source_time_start, source_time_end, expand_hint, metadata_json, created_at
                 ) VALUES (
                    'summary-child', 'codex', 'session-a', 'session-a', 0,
                    'canonical child summary', 'summary-child-hash', 3, 3,
                    11, 11, NULL, NULL, 13
                 );
                 INSERT INTO lcm_summary_nodes(
                    node_id, provider, conversation_id, session_id, depth, summary_text,
                    summary_hash, summary_token_count, source_token_count,
                    source_time_start, source_time_end, expand_hint, metadata_json, created_at
                 ) VALUES (
                    'summary-parent', 'codex', 'session-a', 'session-a', 1,
                    'canonical parent summary', 'summary-parent-hash', 3, 6,
                    11, 12, NULL, NULL, 14
                 );
                 INSERT INTO lcm_summary_sources(node_id, source_kind, source_id, ordinal)
                 VALUES ('summary-child', 'raw_message', '11', 0);
                 INSERT INTO lcm_summary_sources(node_id, source_kind, source_id, ordinal)
                 VALUES ('summary-parent', 'summary_node', 'summary-child', 0);
                 INSERT INTO lcm_summary_sources(node_id, source_kind, source_id, ordinal)
                 VALUES ('summary-parent', 'raw_message', '12', 1);",
                byte_count = external_content.len(),
                char_count = external_content.chars().count(),
            ))
            .await?;
        Ok(())
    }
}

#[cfg(test)]
async fn open_registered_test_database(
    path: &std::path::Path,
) -> tracedecay_runtime_core::errors::Result<Arc<RegisteredGlobalDb>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let authority = tracedecay_runtime_core::db::DatabaseAuthority::acquire_test(
        path,
        "open registered global-db test runtime",
    )?;
    let (database, _) = tracedecay_runtime_core::db::Database::publish_test_runtime(
        path,
        &authority,
        tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Initialize,
    )
    .await?;
    crate::ensure_registered_schema(database.conn()).await?;
    let runtime = database.retained_runtime().clone();
    let expected_binding = runtime.binding().clone();
    let expected_locator = runtime.locator().verified().clone();
    let authority = runtime
        .database_authority("attach registered global-db test runtime")
        .map_err(
            |failure| tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "attach registered global-db test runtime".to_owned(),
                message: format!("{failure:?}"),
            },
        )?;
    Ok(Arc::new(
        RegisteredGlobalDb::migrate_and_attach(
            runtime,
            expected_binding,
            expected_locator,
            authority,
        )
        .await?,
    ))
}

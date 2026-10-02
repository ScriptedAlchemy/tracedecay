//! Transcript and session-ingest test adapters for the registered host runtime.

use std::path::Path;

use super::{HostAdmissionScope, HostAdmissionTestRuntimeV1};

impl HostAdmissionTestRuntimeV1 {
    #[doc(hidden)]
    pub async fn session_activity_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_domain::errors::Result<
        tracedecay_automation_runtime::automation::scheduler::SessionActivity,
    > {
        Ok(
            tracedecay_automation_runtime::automation::scheduler::load_session_activity(
                self.session_database_for_test(scope)?,
            )
            .await,
        )
    }

    #[doc(hidden)]
    pub fn transcript_store_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_domain::errors::Result<
        tracedecay_session_memory::transcript::GlobalDbTranscriptStore<
            &'_ tracedecay_global_db::RegisteredGlobalDb,
        >,
    > {
        Ok(
            tracedecay_session_memory::transcript::GlobalDbTranscriptStore::new(
                self.session_database_for_test(scope)?,
            ),
        )
    }

    #[doc(hidden)]
    pub async fn parse_offset_for_test(
        &self,
        scope: HostAdmissionScope,
        path: &str,
    ) -> tracedecay_domain::errors::Result<Option<tracedecay_global_db::ParseOffset>> {
        self.session_database_for_test(scope)?
            .get_parse_offset(path)
            .await
            .map_err(
                |error| tracedecay_domain::errors::TraceDecayError::Database {
                    operation: "load registered parse offset".to_owned(),
                    message: error.to_string(),
                },
            )
    }

    #[doc(hidden)]
    pub async fn set_parse_offset_for_test(
        &self,
        scope: HostAdmissionScope,
        path: &str,
        offset: tracedecay_global_db::ParseOffset,
    ) -> tracedecay_domain::errors::Result<()> {
        self.session_database_for_test(scope)?
            .set_parse_offset(path, offset)
            .await
            .map_err(
                |message| tracedecay_domain::errors::TraceDecayError::Database {
                    operation: "set retained test parse offset".to_owned(),
                    message,
                },
            )
    }

    #[doc(hidden)]
    pub async fn session_message_count_for_test(
        &self,
        scope: HostAdmissionScope,
        project_key: Option<&str>,
    ) -> tracedecay_domain::errors::Result<i64> {
        let database = self.session_database_for_test(scope)?;
        let result = match project_key {
            Some(project_key) => {
                database
                    .session_message_count_for_project(project_key)
                    .await
            }
            None => database.session_message_count().await,
        };
        result.map_err(
            |message| tracedecay_domain::errors::TraceDecayError::Database {
                operation: "count registered session messages".to_owned(),
                message,
            },
        )
    }

    #[doc(hidden)]
    pub async fn session_ingest_health_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: Option<&str>,
    ) -> tracedecay_domain::errors::Result<tracedecay_global_db::SessionIngestHealth> {
        self.session_database_for_test(scope)?
            .session_ingest_health_for_provider(provider)
            .await
            .map_err(
                |message| tracedecay_domain::errors::TraceDecayError::Database {
                    operation: "read registered session ingest health".to_owned(),
                    message,
                },
            )
    }

    #[doc(hidden)]
    pub async fn set_project_parse_offset_for_test(
        &self,
        path: &str,
        offset: tracedecay_global_db::ParseOffset,
    ) -> tracedecay_domain::errors::Result<()> {
        self.project_database_for_test()?
            .advance_parse_offset_result(path, offset)
            .await
            .map_err(
                |error| tracedecay_domain::errors::TraceDecayError::Database {
                    operation: "write registered project parse offset test seed".to_owned(),
                    message: error.to_string(),
                },
            )
    }

    #[doc(hidden)]
    pub async fn search_session_messages_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: &str,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
    ) -> tracedecay_domain::errors::Result<
        Vec<tracedecay_sessions::runtime::SessionMessageSearchResult>,
    > {
        self.session_database_for_test(scope)?
            .search_session_messages(provider, project_key, query, limit)
            .await
    }

    #[doc(hidden)]
    pub async fn search_session_messages_filtered_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: &str,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
        filters: tracedecay_sessions::runtime::SessionSearchFilters<'_>,
    ) -> tracedecay_domain::errors::Result<
        Vec<tracedecay_sessions::runtime::SessionMessageSearchResult>,
    > {
        let fetch_limit = limit.saturating_mul(16).max(limit);
        let mut results = self
            .session_database_for_test(scope)?
            .search_session_messages(provider, project_key, query, fetch_limit)
            .await?;
        results.retain(|result| {
            let scope_matches = match filters.scope {
                tracedecay_sessions::runtime::SessionSearchScope::All => true,
                tracedecay_sessions::runtime::SessionSearchScope::ParentsOnly => {
                    !result.session.is_subagent
                }
                tracedecay_sessions::runtime::SessionSearchScope::SubagentsOnly => {
                    result.session.is_subagent
                }
            };
            let tool_result = result.message.role == "tool"
                || matches!(
                    result.message.kind.as_deref(),
                    Some("tool_result" | "tool_output")
                )
                || result
                    .message
                    .metadata_json
                    .as_deref()
                    .and_then(|metadata| serde_json::from_str::<serde_json::Value>(metadata).ok())
                    .and_then(|metadata| metadata.get("tool_events").cloned())
                    .and_then(|events| events.as_array().cloned())
                    .is_some_and(|events| {
                        events.iter().any(|event| {
                            event.get("type").and_then(serde_json::Value::as_str)
                                == Some("tool_result")
                        })
                    });
            let message_type_matches = match filters.message_type {
                tracedecay_sessions::runtime::SessionMessageType::All => true,
                tracedecay_sessions::runtime::SessionMessageType::DirectUser => {
                    result.message.role == "user" && !tool_result
                }
                tracedecay_sessions::runtime::SessionMessageType::ToolResult => tool_result,
            };
            let parent_matches = filters
                .parent_session_id
                .is_none_or(|parent| result.session.parent_session_id.as_deref() == Some(parent));
            let time_matches =
                filters.time_range.start_time.is_none_or(|start| {
                    result.message.timestamp.is_some_and(|value| value >= start)
                }) && filters
                    .time_range
                    .end_time
                    .is_none_or(|end| result.message.timestamp.is_some_and(|value| value <= end));
            scope_matches && message_type_matches && parent_matches && time_matches
        });
        results.truncate(limit);
        Ok(results)
    }

    #[doc(hidden)]
    #[allow(
        clippy::too_many_arguments,
        reason = "The test boundary varies provider, project, Git and message filters independently to verify isolation."
    )]
    pub async fn search_session_messages_git_scoped_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: Option<&str>,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
        filters: tracedecay_sessions::runtime::SessionSearchFilters<'_>,
        git_filter: &tracedecay_sessions::runtime::git_correlation::GitScopeFilter,
    ) -> tracedecay_domain::errors::Result<
        Vec<tracedecay_sessions::runtime::SessionMessageSearchResult>,
    > {
        let provider =
            provider.ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Database {
                operation: "search registered git-scoped session messages".to_owned(),
                message: "test facade requires an exact provider".to_owned(),
            })?;
        let database = self.session_database_for_test(scope)?;
        let resolve_error =
            |message: String| tracedecay_domain::errors::TraceDecayError::Database {
                operation: "resolve registered git-scoped sessions".to_owned(),
                message,
            };
        let scoped_ids = match tracedecay_global_db::GlobalDbGitCorrelationStore::new(database)
            .session_ids_for_scope(git_filter, None)
            .await
        {
            Ok(Some(ids)) => ids,
            Ok(None) => {
                return Err(resolve_error(
                    "Git scope resolution requires a non-empty filter".to_owned(),
                ));
            }
            // A project that never recorded Git evidence has no session in
            // any Git scope.
            Err(
                tracedecay_sessions::runtime::git_correlation::GitCorrelationError::Unavailable(_),
            ) => Vec::new(),
            Err(error) => return Err(resolve_error(error.to_string())),
        };
        let mut results = self
            .search_session_messages_filtered_for_test(
                scope,
                provider,
                project_key,
                query,
                limit.saturating_mul(16).max(limit),
                filters,
            )
            .await?;
        results.retain(|result| {
            scoped_ids.iter().any(|(candidate_provider, session_id)| {
                (candidate_provider.is_empty() || candidate_provider == &result.session.provider)
                    && session_id == &result.session.session_id
            })
        });
        results.truncate(limit);
        Ok(results)
    }

    #[doc(hidden)]
    pub async fn set_session_message_projection_failure_for_test(
        &self,
        scope: HostAdmissionScope,
        enabled: bool,
    ) -> tracedecay_domain::errors::Result<()> {
        let writer = self.session_database_for_test(scope)?.writer_connection()?;
        let statement = if enabled {
            "CREATE TRIGGER fail_session_message_projection
             BEFORE INSERT ON lcm_raw_messages
             BEGIN
                SELECT RAISE(ABORT, 'projection failure');
             END;"
        } else {
            "DROP TRIGGER IF EXISTS fail_session_message_projection;"
        };
        writer.execute_batch(statement).await.map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::Database {
                operation: "set registered session projection failure fixture".to_owned(),
                message: error.to_string(),
            }
        })
    }

    /// Runs one selected provider through the exact registered project authority.
    #[doc(hidden)]
    pub async fn ingest_project_provider_for_test(
        &self,
        project_root: &Path,
        provider: Option<tracedecay_sessions::runtime::SessionProvider>,
    ) -> tracedecay_domain::errors::Result<
        tracedecay_sessions::runtime::shared::TranscriptIngestStats,
    > {
        let project_id = self.project_id.as_ref().ok_or_else(|| {
            tracedecay_domain::errors::TraceDecayError::Database {
                operation: "ingest registered project provider test fixture".to_owned(),
                message: "registered project identity is unavailable".to_owned(),
            }
        })?;
        let database = self.project_database_for_test()?;
        let authority = tracedecay_host_admission::session_ingest_authority::GlobalDbSessionIngestAuthority::new(database)
            .with_background_cpu(self.background_cpu());
        Ok(
            tracedecay_sessions::runtime::ingest_project_sources_for_provider(
                &self.brain_id,
                &self.profile_id,
                &authority,
                project_root,
                Some(project_id.clone()),
                provider,
                true,
            )
            .await
            .stats,
        )
    }

    #[doc(hidden)]
    pub async fn project_parse_offset_for_test(
        &self,
        path: &str,
    ) -> tracedecay_domain::errors::Result<Option<tracedecay_global_db::ParseOffset>> {
        self.project_database_for_test()?
            .get_parse_offset(path)
            .await
            .map_err(
                |error| tracedecay_domain::errors::TraceDecayError::Database {
                    operation: "load registered parse offset".to_owned(),
                    message: error.to_string(),
                },
            )
    }

    #[doc(hidden)]
    pub async fn project_session_for_test(
        &self,
        provider: &str,
        session_id: &str,
    ) -> tracedecay_domain::errors::Result<Option<tracedecay_sessions::runtime::SessionRecord>>
    {
        self.project_database_for_test()?
            .get_session(provider, session_id)
            .await
            .map_err(
                |error| tracedecay_domain::errors::TraceDecayError::Database {
                    operation: "load registered session".to_owned(),
                    message: error.to_string(),
                },
            )
    }

    #[doc(hidden)]
    pub async fn project_session_message_for_test(
        &self,
        provider: &str,
        message_id: &str,
    ) -> tracedecay_domain::errors::Result<Option<tracedecay_sessions::runtime::SessionMessageRecord>>
    {
        self.project_database_for_test()?
            .get_session_message(provider, message_id)
            .await
    }

    #[doc(hidden)]
    pub async fn search_project_session_messages_for_test(
        &self,
        provider: &str,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
    ) -> tracedecay_domain::errors::Result<
        Vec<tracedecay_sessions::runtime::SessionMessageSearchResult>,
    > {
        self.project_database_for_test()?
            .search_session_messages(provider, project_key, query, limit)
            .await
    }

    #[doc(hidden)]
    pub async fn recent_project_session_goals_for_test(
        &self,
        project_key: &str,
        limit: usize,
    ) -> tracedecay_domain::errors::Result<
        Vec<tracedecay_sessions::runtime::SessionMessageSearchResult>,
    > {
        self.project_database_for_test()?
            .recent_session_goals(Some(project_key), limit)
            .await
    }

    #[doc(hidden)]
    pub async fn project_lcm_raw_message_for_test(
        &self,
        provider: &str,
        message_id: &str,
    ) -> tracedecay_domain::errors::Result<Option<tracedecay_lcm::LcmRawMessage>> {
        let database = self.project_database_for_test()?;
        let snapshot = database.read_snapshot().await?;
        tracedecay_lcm::schema::load_raw_message(&snapshot, provider, message_id)
            .await
            .map_err(
                |error| tracedecay_domain::errors::TraceDecayError::Database {
                    operation: "read project LCM raw message fixture".to_owned(),
                    message: error.to_string(),
                },
            )
    }

    /// Live `lcm_raw_messages` store ids for one provider session, in store order.
    #[doc(hidden)]
    pub async fn lcm_raw_message_store_ids_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: &str,
        session_id: &str,
    ) -> tracedecay_domain::errors::Result<Vec<i64>> {
        let snapshot = self
            .session_database_for_test(scope)?
            .read_snapshot()
            .await?;
        let mut rows = snapshot
            .query(
                "SELECT store_id FROM lcm_raw_messages
                 WHERE provider = ?1 AND session_id = ?2
                 ORDER BY store_id",
                tracedecay_runtime_core::db::engine::params![provider, session_id],
            )
            .await
            .map_err(
                |error| tracedecay_domain::errors::TraceDecayError::Database {
                    operation: "query registered LCM raw message store ids".to_owned(),
                    message: error.to_string(),
                },
            )?;
        let mut store_ids = Vec::new();
        while let Some(row) = rows.next().await.map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::Database {
                operation: "read registered LCM raw message store ids".to_owned(),
                message: error.to_string(),
            }
        })? {
            store_ids.push(row.get::<i64>(0).map_err(|error| {
                tracedecay_domain::errors::TraceDecayError::Database {
                    operation: "decode registered LCM raw message store id".to_owned(),
                    message: error.to_string(),
                }
            })?);
        }
        Ok(store_ids)
    }

    /// Installs or removes the deterministic projection-failure trigger in-place.
    #[doc(hidden)]
    pub async fn set_project_projection_failure_for_test(
        &self,
        enabled: bool,
    ) -> tracedecay_domain::errors::Result<()> {
        let statement = if enabled {
            "CREATE TRIGGER fail_session_message_projection
             BEFORE INSERT ON lcm_raw_messages
             BEGIN
                SELECT RAISE(ABORT, 'projection failure');
             END;"
        } else {
            "DROP TRIGGER IF EXISTS fail_session_message_projection;
             DROP TRIGGER IF EXISTS fail_claude_suffix_projection;"
        };
        self.project_database_for_test()?
            .writer_connection()?
            .execute_batch(statement)
            .await
            .map_err(
                |error| tracedecay_domain::errors::TraceDecayError::Database {
                    operation: "configure registered project projection failure".to_owned(),
                    message: error.to_string(),
                },
            )
    }
}

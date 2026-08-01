//! Transcript and session-ingest test adapters for the registered host runtime.

use std::path::Path;

use super::{HostAdmissionScope, HostAdmissionTestRuntimeV1};

impl HostAdmissionTestRuntimeV1 {
    #[doc(hidden)]
    pub async fn ensure_session_cursor_key_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> crate::errors::Result<tracedecay_domain::SignedCursorKeyRefV1> {
        self.session_database_for_test(scope)?
            .ensure_active_session_cursor_key_result()
            .await
            .map_err(|error| crate::errors::TraceDecayError::Database {
                operation: "provision test session cursor authentication key".to_owned(),
                message: error.to_string(),
            })
    }

    #[doc(hidden)]
    pub async fn session_activity_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> crate::errors::Result<crate::automation::scheduler::SessionActivity> {
        Ok(crate::automation::scheduler::load_session_activity(
            self.session_database_for_test(scope)?,
        )
        .await)
    }

    #[doc(hidden)]
    pub fn transcript_store_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> crate::errors::Result<
        crate::store::GlobalDbTranscriptStore<&'_ crate::global_db::RegisteredGlobalDb>,
    > {
        Ok(crate::store::GlobalDbTranscriptStore::new(
            self.session_database_for_test(scope)?,
        ))
    }

    #[doc(hidden)]
    pub async fn parse_offset_for_test(
        &self,
        scope: HostAdmissionScope,
        path: &str,
    ) -> crate::errors::Result<Option<crate::global_db::ParseOffset>> {
        Ok(self
            .session_database_for_test(scope)?
            .get_parse_offset(path)
            .await)
    }

    #[doc(hidden)]
    pub async fn set_parse_offset_for_test(
        &self,
        scope: HostAdmissionScope,
        path: &str,
        offset: crate::global_db::ParseOffset,
    ) -> crate::errors::Result<()> {
        self.session_database_for_test(scope)?
            .set_parse_offset(path, offset)
            .await
            .map_err(|message| crate::errors::TraceDecayError::Database {
                operation: "set retained test parse offset".to_owned(),
                message,
            })
    }

    #[doc(hidden)]
    pub async fn session_message_count_for_test(
        &self,
        scope: HostAdmissionScope,
        project_key: Option<&str>,
    ) -> crate::errors::Result<i64> {
        let database = self.session_database_for_test(scope)?;
        let result = match project_key {
            Some(project_key) => {
                database
                    .session_message_count_for_project(project_key)
                    .await
            }
            None => database.session_message_count().await,
        };
        result.map_err(|message| crate::errors::TraceDecayError::Database {
            operation: "count registered session messages".to_owned(),
            message,
        })
    }

    #[doc(hidden)]
    pub async fn session_ingest_health_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: Option<&str>,
    ) -> crate::errors::Result<crate::global_db::SessionIngestHealth> {
        self.session_database_for_test(scope)?
            .session_ingest_health_for_provider(provider)
            .await
            .map_err(|message| crate::errors::TraceDecayError::Database {
                operation: "read registered session ingest health".to_owned(),
                message,
            })
    }

    #[doc(hidden)]
    pub async fn set_parse_offset_insert_failure_for_test(
        &self,
        scope: HostAdmissionScope,
        enabled: bool,
    ) -> crate::errors::Result<()> {
        let statement = if enabled {
            "CREATE TRIGGER fail_parse_offset_insert
             BEFORE INSERT ON parse_offsets
             BEGIN
                SELECT RAISE(ABORT, 'late parse offset failure');
             END;"
        } else {
            "DROP TRIGGER IF EXISTS fail_parse_offset_insert;"
        };
        self.session_database_for_test(scope)?
            .writer_connection()?
            .execute_batch(statement)
            .await
            .map_err(|error| crate::errors::TraceDecayError::Database {
                operation: "configure registered parse-offset failure".to_owned(),
                message: error.to_string(),
            })
    }

    #[doc(hidden)]
    pub async fn seed_transcript_backfill_for_test(
        &self,
        scope: HostAdmissionScope,
        session: &crate::sessions::SessionRecord,
        messages: &[crate::sessions::SessionMessageRecord],
    ) -> crate::errors::Result<()> {
        if !self.upsert_session_for_test(scope, session).await? {
            return Err(crate::errors::TraceDecayError::Database {
                operation: "seed transcript backfill session fixture".to_owned(),
                message: "registered session fixture write failed".to_owned(),
            });
        }
        for message in messages {
            if !self.upsert_session_message_for_test(scope, message).await? {
                return Err(crate::errors::TraceDecayError::Database {
                    operation: "seed transcript backfill message fixture".to_owned(),
                    message: format!(
                        "registered message fixture write failed for {}/{}",
                        message.provider, message.message_id
                    ),
                });
            }
        }
        self.session_database_for_test(scope)?
            .writer_connection()?
            .execute(
                "DELETE FROM session_schema_migrations
                 WHERE name = 'transcript_facts_backfill'",
                (),
            )
            .await
            .map_err(|error| crate::errors::TraceDecayError::Database {
                operation: "clear transcript backfill fixture marker".to_owned(),
                message: error.to_string(),
            })?;
        Ok(())
    }

    #[doc(hidden)]
    pub async fn transcript_backfill_marker_version_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> crate::errors::Result<Option<i64>> {
        let snapshot = self
            .session_database_for_test(scope)?
            .read_snapshot()
            .await?;
        let mut rows = snapshot
            .query(
                "SELECT version FROM session_schema_migrations
                 WHERE name = 'transcript_facts_backfill'",
                (),
            )
            .await
            .map_err(|error| crate::errors::TraceDecayError::Database {
                operation: "query transcript backfill marker".to_owned(),
                message: error.to_string(),
            })?;
        let Some(row) =
            rows.next()
                .await
                .map_err(|error| crate::errors::TraceDecayError::Database {
                    operation: "read transcript backfill marker".to_owned(),
                    message: error.to_string(),
                })?
        else {
            return Ok(None);
        };
        row.get::<i64>(0)
            .map(Some)
            .map_err(|error| crate::errors::TraceDecayError::Database {
                operation: "decode transcript backfill marker".to_owned(),
                message: error.to_string(),
            })
    }

    #[doc(hidden)]
    pub async fn set_project_parse_offset_for_test(
        &self,
        path: &str,
        offset: crate::global_db::ParseOffset,
    ) -> crate::errors::Result<()> {
        self.project_database_for_test()?
            .advance_parse_offset_result(path, offset)
            .await
            .map_err(|error| crate::errors::TraceDecayError::Database {
                operation: "write registered project parse offset test seed".to_owned(),
                message: error.to_string(),
            })
    }

    #[doc(hidden)]
    pub async fn ingest_profile_transcript_source_for_test(
        &self,
        source: &dyn crate::sessions::source::TranscriptSource,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> crate::sessions::source::TranscriptIngestResult<
        crate::sessions::shared::TranscriptIngestStats,
    > {
        let database = self
            .session_database_for_test(HostAdmissionScope::Profile)
            .map_err(
                |error| crate::sessions::source::TranscriptIngestError::ScanIo {
                    operation: "bind registered profile session test runtime",
                    path: project_root.to_path_buf(),
                    source: std::io::Error::other(error.to_string()),
                },
            )?;
        let store = crate::store::GlobalDbTranscriptStore::new(database);
        crate::sessions::source::try_ingest_source(&store, source, project_root, max_new_bytes)
            .await
    }

    #[doc(hidden)]
    pub async fn search_session_messages_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: &str,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
    ) -> crate::errors::Result<Vec<crate::sessions::SessionMessageSearchResult>> {
        Ok(self
            .session_database_for_test(scope)?
            .search_session_messages(provider, project_key, query, limit)
            .await)
    }

    #[doc(hidden)]
    pub async fn search_session_messages_filtered_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: &str,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
        filters: crate::sessions::SessionSearchFilters<'_>,
    ) -> crate::errors::Result<Vec<crate::sessions::SessionMessageSearchResult>> {
        let fetch_limit = limit.saturating_mul(16).max(limit);
        let mut results = self
            .session_database_for_test(scope)?
            .search_session_messages(provider, project_key, query, fetch_limit)
            .await;
        results.retain(|result| {
            let scope_matches = match filters.scope {
                crate::sessions::SessionSearchScope::All => true,
                crate::sessions::SessionSearchScope::ParentsOnly => !result.session.is_subagent,
                crate::sessions::SessionSearchScope::SubagentsOnly => result.session.is_subagent,
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
                crate::sessions::SessionMessageType::All => true,
                crate::sessions::SessionMessageType::DirectUser => {
                    result.message.role == "user" && !tool_result
                }
                crate::sessions::SessionMessageType::ToolResult => tool_result,
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
    pub async fn search_session_messages_git_scoped_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: Option<&str>,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
        filters: crate::sessions::SessionSearchFilters<'_>,
        git_filter: &crate::sessions::git_correlation::GitScopeFilter,
    ) -> crate::errors::Result<Vec<crate::sessions::SessionMessageSearchResult>> {
        let provider = provider.ok_or_else(|| crate::errors::TraceDecayError::Database {
            operation: "search registered git-scoped session messages".to_owned(),
            message: "test facade requires an exact provider".to_owned(),
        })?;
        let database = self.session_database_for_test(scope)?;
        let snapshot = database.read_snapshot().await?;
        let scoped_ids =
            crate::sessions::git_correlation::session_ids_for_scope(&snapshot, git_filter)
                .await
                .map_err(|error| crate::errors::TraceDecayError::Database {
                    operation: "resolve registered git-scoped sessions".to_owned(),
                    message: error.to_string(),
                })?;
        drop(snapshot);
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
        if let Some(scoped_ids) = scoped_ids {
            results.retain(|result| {
                scoped_ids.iter().any(|(candidate_provider, session_id)| {
                    (candidate_provider.is_empty()
                        || candidate_provider == &result.session.provider)
                        && session_id == &result.session.session_id
                })
            });
        }
        results.truncate(limit);
        Ok(results)
    }

    #[doc(hidden)]
    pub async fn set_session_message_projection_failure_for_test(
        &self,
        scope: HostAdmissionScope,
        enabled: bool,
    ) -> crate::errors::Result<()> {
        let writer = self.session_database_for_test(scope)?.writer_connection()?;
        let statement = if enabled {
            "CREATE TRIGGER fail_session_message_projection
             BEFORE INSERT ON session_messages
             BEGIN
                SELECT RAISE(ABORT, 'projection failure');
             END;"
        } else {
            "DROP TRIGGER IF EXISTS fail_session_message_projection;"
        };
        writer.execute_batch(statement).await.map_err(|error| {
            crate::errors::TraceDecayError::Database {
                operation: "set registered session projection failure fixture".to_owned(),
                message: error.to_string(),
            }
        })
    }

    /// Drives one transcript source through the retained ProjectSessions mount.
    #[doc(hidden)]
    pub async fn ingest_project_transcript_source_for_test(
        &self,
        source: &dyn crate::sessions::source::TranscriptSource,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> crate::sessions::source::TranscriptIngestResult<
        crate::sessions::shared::TranscriptIngestStats,
    > {
        let database = self.project_database_for_test().map_err(|error| {
            crate::sessions::source::TranscriptIngestError::ScanIo {
                operation: "bind registered project session test runtime",
                path: project_root.to_path_buf(),
                source: std::io::Error::other(error.to_string()),
            }
        })?;
        let store = crate::store::GlobalDbTranscriptStore::new(database);
        crate::sessions::source::try_ingest_source(&store, source, project_root, max_new_bytes)
            .await
    }

    /// Runs one selected provider through the exact registered project authority.
    #[doc(hidden)]
    pub async fn ingest_project_provider_for_test(
        &self,
        project_root: &Path,
        provider: Option<crate::sessions::SessionProvider>,
    ) -> crate::errors::Result<crate::sessions::shared::TranscriptIngestStats> {
        let project_id =
            self.project_id
                .as_ref()
                .ok_or_else(|| crate::errors::TraceDecayError::Database {
                    operation: "ingest registered project provider test fixture".to_owned(),
                    message: "registered project identity is unavailable".to_owned(),
                })?;
        let database = self.project_database_for_test()?;
        let authority = crate::store::GlobalDbSessionIngestAuthority::new(database);
        Ok(crate::sessions::ingest_project_sources_for_provider(
            &self.brain_id,
            &self.profile_id,
            &authority,
            project_root,
            Some(project_id.clone()),
            provider,
            true,
        )
        .await
        .stats)
    }

    #[doc(hidden)]
    pub async fn project_parse_offset_for_test(
        &self,
        path: &str,
    ) -> crate::errors::Result<Option<crate::global_db::ParseOffset>> {
        Ok(self
            .project_database_for_test()?
            .get_parse_offset(path)
            .await)
    }

    #[doc(hidden)]
    pub async fn project_session_for_test(
        &self,
        provider: &str,
        session_id: &str,
    ) -> crate::errors::Result<Option<crate::sessions::SessionRecord>> {
        Ok(self
            .project_database_for_test()?
            .get_session(provider, session_id)
            .await)
    }

    #[doc(hidden)]
    pub async fn project_session_message_for_test(
        &self,
        provider: &str,
        message_id: &str,
    ) -> crate::errors::Result<Option<crate::sessions::SessionMessageRecord>> {
        Ok(self
            .project_database_for_test()?
            .get_session_message(provider, message_id)
            .await)
    }

    #[doc(hidden)]
    pub async fn search_project_session_messages_for_test(
        &self,
        provider: &str,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
    ) -> crate::errors::Result<Vec<crate::sessions::SessionMessageSearchResult>> {
        Ok(self
            .project_database_for_test()?
            .search_session_messages(provider, project_key, query, limit)
            .await)
    }

    #[doc(hidden)]
    pub async fn recent_project_session_goals_for_test(
        &self,
        project_key: &str,
        limit: usize,
    ) -> crate::errors::Result<Vec<crate::sessions::SessionMessageSearchResult>> {
        Ok(self
            .project_database_for_test()?
            .recent_session_goals(Some(project_key), limit)
            .await)
    }

    #[doc(hidden)]
    pub async fn project_lcm_raw_message_for_test(
        &self,
        provider: &str,
        message_id: &str,
    ) -> crate::errors::Result<Option<crate::sessions::lcm::LcmRawMessage>> {
        Ok(self
            .project_database_for_test()?
            .lcm_load_raw_message(provider, message_id)
            .await)
    }

    /// Live `lcm_raw_messages` store ids for one provider session, in store order.
    #[doc(hidden)]
    pub async fn lcm_raw_message_store_ids_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: &str,
        session_id: &str,
    ) -> crate::errors::Result<Vec<i64>> {
        let snapshot = self
            .session_database_for_test(scope)?
            .read_snapshot()
            .await?;
        let mut rows = snapshot
            .query(
                "SELECT store_id FROM lcm_raw_messages
                 WHERE provider = ?1 AND session_id = ?2
                 ORDER BY store_id",
                crate::db::engine::params![provider, session_id],
            )
            .await
            .map_err(|error| crate::errors::TraceDecayError::Database {
                operation: "query registered LCM raw message store ids".to_owned(),
                message: error.to_string(),
            })?;
        let mut store_ids = Vec::new();
        while let Some(row) =
            rows.next()
                .await
                .map_err(|error| crate::errors::TraceDecayError::Database {
                    operation: "read registered LCM raw message store ids".to_owned(),
                    message: error.to_string(),
                })?
        {
            store_ids.push(row.get::<i64>(0).map_err(|error| {
                crate::errors::TraceDecayError::Database {
                    operation: "decode registered LCM raw message store id".to_owned(),
                    message: error.to_string(),
                }
            })?);
        }
        Ok(store_ids)
    }

    #[doc(hidden)]
    pub async fn project_parse_offset_by_suffix_for_test(
        &self,
        suffix: &str,
    ) -> crate::errors::Result<Option<crate::global_db::ParseOffset>> {
        let snapshot = self.project_database_for_test()?.read_snapshot().await?;
        let mut rows = snapshot
            .query(
                "SELECT byte_offset, mtime, file_id
                 FROM parse_offsets
                 WHERE file_path LIKE '%' || ?1
                 ORDER BY file_path
                 LIMIT 1",
                crate::db::engine::params![suffix],
            )
            .await
            .map_err(|error| crate::errors::TraceDecayError::Database {
                operation: "query registered project parse offset by suffix".to_owned(),
                message: error.to_string(),
            })?;
        let Some(row) =
            rows.next()
                .await
                .map_err(|error| crate::errors::TraceDecayError::Database {
                    operation: "read registered project parse offset by suffix".to_owned(),
                    message: error.to_string(),
                })?
        else {
            return Ok(None);
        };
        let decode = |index| {
            row.get::<i64>(index)
                .map(|value| u64::try_from(value).unwrap_or_default())
                .map_err(|error| crate::errors::TraceDecayError::Database {
                    operation: "decode registered project parse offset by suffix".to_owned(),
                    message: error.to_string(),
                })
        };
        Ok(Some(crate::global_db::ParseOffset {
            byte_offset: decode(0)?,
            mtime: decode(1)?,
            file_id: decode(2)?,
        }))
    }

    /// Installs or removes the deterministic projection-failure trigger in-place.
    #[doc(hidden)]
    pub async fn set_project_projection_failure_for_test(
        &self,
        enabled: bool,
    ) -> crate::errors::Result<()> {
        let statement = if enabled {
            "CREATE TRIGGER fail_session_message_projection
             BEFORE INSERT ON session_messages
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
            .map_err(|error| crate::errors::TraceDecayError::Database {
                operation: "configure registered project projection failure".to_owned(),
                message: error.to_string(),
            })
    }
}

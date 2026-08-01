use super::*;

impl HostAdmissionTestRuntimeV1 {
    #[doc(hidden)]
    pub async fn append_analytics_event_for_test(
        &self,
        scope: HostAdmissionScope,
        event: &crate::global_db::AnalyticsEventInsert,
    ) -> Result<i64> {
        let database = match scope {
            HostAdmissionScope::Project => self.project_database_for_test()?,
            HostAdmissionScope::Profile => self.profile_database.as_ref(),
        };
        database
            .append_analytics_event(event)
            .await
            .map_err(|message| TraceDecayError::Database {
                operation: "append registered analytics event".to_owned(),
                message,
            })
    }

    #[doc(hidden)]
    pub async fn append_analytics_events_for_test(
        &self,
        scope: HostAdmissionScope,
        events: &[crate::global_db::AnalyticsEventInsert],
    ) -> Result<Vec<i64>> {
        let database = match scope {
            HostAdmissionScope::Project => self.project_database_for_test()?,
            HostAdmissionScope::Profile => self.profile_database.as_ref(),
        };
        database
            .append_analytics_events(events)
            .await
            .map_err(|message| TraceDecayError::Database {
                operation: "append registered analytics event batch".to_owned(),
                message,
            })
    }

    #[doc(hidden)]
    pub async fn append_profile_analytics_events_for_test(
        &self,
        events: &[crate::global_db::AnalyticsEventInsert],
    ) -> Result<Vec<i64>> {
        self.append_analytics_events_for_test(HostAdmissionScope::Profile, events)
            .await
    }

    #[doc(hidden)]
    pub async fn import_profile_hook_analytics_for_test(
        &self,
        sources: &[crate::analytics_bridge::HookImportSource],
    ) -> crate::analytics_bridge::HookImportOutcome {
        crate::analytics_bridge::import_hook_analytics(
            self.profile_database.as_ref(),
            sources.to_vec(),
        )
        .await
    }

    #[doc(hidden)]
    pub async fn profile_analytics_indexes_present_for_test(&self) -> Result<i64> {
        let snapshot = self.profile_database.read_snapshot().await?;
        let mut rows = snapshot
            .query(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index'
                   AND name IN (
                       'idx_analytics_events_project_time',
                       'idx_analytics_events_timestamp'
                   )",
                (),
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "query registered profile analytics indexes".to_owned(),
                message: error.to_string(),
            })?;
        let row = rows
            .next()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "read registered profile analytics indexes".to_owned(),
                message: error.to_string(),
            })?
            .ok_or_else(|| TraceDecayError::Database {
                operation: "read registered profile analytics indexes".to_owned(),
                message: "count query returned no row".to_owned(),
            })?;
        row.get(0).map_err(|error| TraceDecayError::Database {
            operation: "decode registered profile analytics indexes".to_owned(),
            message: error.to_string(),
        })
    }

    /// Fails the calling test loudly: a read this runtime could not perform is
    /// not a token total of zero.
    #[doc(hidden)]
    pub async fn get_project_tokens(&self, project_path: &Path) -> u64 {
        self.profile_database
            .try_get_project_tokens(project_path)
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "could not read project tokens for '{}': {error}",
                    project_path.display()
                )
            })
    }

    #[doc(hidden)]
    pub async fn global_tokens_saved(&self) -> Option<u64> {
        self.profile_database.global_tokens_saved().await
    }

    #[doc(hidden)]
    pub async fn record_savings_for_test(
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

    #[doc(hidden)]
    pub async fn sum_savings_for_test(
        &self,
        project: Option<&str>,
        since: i64,
    ) -> crate::global_db::SavingsTotal {
        self.profile_database.sum_savings(project, since).await
    }

    #[doc(hidden)]
    pub async fn savings_history_for_test(
        &self,
        project: Option<&str>,
        since: i64,
    ) -> Vec<crate::global_db::SavingsDay> {
        self.profile_database.savings_history(project, since).await
    }

    #[doc(hidden)]
    pub async fn insert_turn_for_test(&self, turn: &crate::types::CostTurn) -> bool {
        self.profile_database.insert_turn(turn).await
    }

    #[doc(hidden)]
    pub async fn insert_turns_for_test(&self, turns: &[crate::types::CostTurn]) -> usize {
        self.profile_database.insert_turns(turns).await
    }

    #[doc(hidden)]
    pub(crate) fn dashboard_test_authority(
        self: &Arc<Self>,
    ) -> Result<crate::dashboard::DashboardHostAdmissionTestAuthorityV1> {
        let project_sessions =
            self.project_registered
                .clone()
                .ok_or_else(|| TraceDecayError::Database {
                    operation: "bind dashboard test project sessions".to_owned(),
                    message: "registered ProjectSessions mount is unavailable".to_owned(),
                })?;
        Ok(
            crate::dashboard::DashboardHostAdmissionTestAuthorityV1::new(
                Arc::clone(self),
                Arc::clone(&self.profile_database),
                project_sessions,
            ),
        )
    }
}

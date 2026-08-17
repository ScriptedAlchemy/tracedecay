use super::*;

impl HostAdmissionTestRuntimeV1 {
    #[doc(hidden)]
    pub async fn lcm_compress_for_test(
        &self,
        request: tracedecay_sessions::runtime::lcm::LcmCompressionRequest,
    ) -> std::result::Result<
        tracedecay_sessions::runtime::lcm::LcmCompressionResponse,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        crate::daemon::lcm_effects::DaemonLcmEffectService::new(
            self.project_registered
                .clone()
                .unwrap_or_else(|| self.profile_registered.clone()),
            None,
            None,
        )
        .compress(request)
        .await
    }

    #[doc(hidden)]
    pub async fn lcm_describe_for_test(
        &self,
        request: tracedecay_sessions::runtime::lcm::LcmDescribeRequest,
    ) -> std::result::Result<
        tracedecay_sessions::runtime::lcm::LcmDescribeResponse,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref())
            .lcm_describe(request)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_expand_for_test(
        &self,
        request: tracedecay_sessions::runtime::lcm::LcmExpandRequest,
    ) -> std::result::Result<
        tracedecay_sessions::runtime::lcm::LcmExpandResponse,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref())
            .lcm_expand(request)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_expand_query_for_test(
        &self,
        request: tracedecay_sessions::runtime::lcm::LcmExpandQueryRequest,
    ) -> std::result::Result<
        tracedecay_sessions::runtime::lcm::LcmExpandQueryResponse,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref())
            .lcm_expand_query(request)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_expand_summary_node_for_test(
        &self,
        provider: &str,
        session_id: &str,
        node_id: &str,
    ) -> std::result::Result<
        tracedecay_sessions::runtime::lcm::LcmSummaryExpansion,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref())
            .lcm_expand_summary_node(provider, session_id, node_id)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_grep_for_test(
        &self,
        request: tracedecay_sessions::runtime::lcm::LcmGrepRequest,
    ) -> std::result::Result<
        tracedecay_sessions::runtime::lcm::LcmGrepOutcome,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref())
            .lcm_grep(request)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_ingest_raw_message_for_test(
        &self,
        scope: HostAdmissionScope,
        message: &tracedecay_sessions::runtime::SessionMessageRecord,
    ) -> std::result::Result<(), tracedecay_sessions::runtime::lcm::LcmError> {
        let database = self
            .session_database_for_test(scope)
            .map_err(|error| tracedecay_sessions::runtime::lcm::LcmError::Db(error.to_string()))?;
        let storage_root = database.db_path().parent().ok_or_else(|| {
            tracedecay_sessions::runtime::lcm::LcmError::Db(
                "registered session database has no storage root".to_string(),
            )
        })?;
        database.lcm_ingest_raw_message(storage_root, message).await
    }

    #[doc(hidden)]
    #[allow(clippy::expect_used)]
    pub async fn lcm_load_raw_message_for_test(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Option<tracedecay_sessions::runtime::lcm::LcmRawMessage> {
        let database = self
            .project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref());
        let snapshot = database
            .read_snapshot()
            .await
            .expect("test raw-message snapshot must remain registered");
        tracedecay_sessions::runtime::lcm::schema::load_raw_message(&snapshot, provider, message_id)
            .await
            .expect("test raw-message load must not hide database or receipt failure")
    }

    #[doc(hidden)]
    pub async fn lcm_load_session_for_test(
        &self,
        request: tracedecay_sessions::runtime::lcm::LcmLoadSessionRequest,
    ) -> std::result::Result<
        tracedecay_sessions::runtime::lcm::LcmLoadSessionPage,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref())
            .lcm_load_session(request)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_preflight_for_test(
        &self,
        request: tracedecay_sessions::runtime::lcm::LcmPreflightRequest,
    ) -> std::result::Result<
        tracedecay_sessions::runtime::lcm::LcmPreflightResponse,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref())
            .lcm_preflight(request)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_recent_sessions_for_test(
        &self,
        provider: Option<&str>,
        limit: usize,
    ) -> std::result::Result<
        Vec<tracedecay_sessions::runtime::lcm::LcmRecentSession>,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref())
            .lcm_recent_sessions(provider, limit)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_run_payload_gc_apply_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: &str,
        session_id: Option<&str>,
        config: &tracedecay_sessions::runtime::lcm::LcmGcConfig,
        now: i64,
    ) -> std::result::Result<
        tracedecay_sessions::runtime::lcm::LcmGcReport,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        let database = self
            .session_database_for_test(scope)
            .map_err(|error| tracedecay_sessions::runtime::lcm::LcmError::Db(error.to_string()))?;
        let storage_root = database.db_path().parent().ok_or_else(|| {
            tracedecay_sessions::runtime::lcm::LcmError::Db(
                "registered session database has no storage root".to_string(),
            )
        })?;
        database
            .lcm_run_payload_gc_apply(storage_root, provider, session_id, config, now)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_session_boundary_for_test(
        &self,
        request: tracedecay_sessions::runtime::lcm::LcmSessionBoundaryRequest,
    ) -> std::result::Result<
        tracedecay_sessions::runtime::lcm::LcmSessionBoundaryResponse,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        crate::daemon::lcm_effects::DaemonLcmEffectService::new(
            self.project_registered
                .clone()
                .unwrap_or_else(|| self.profile_registered.clone()),
            None,
            None,
        )
        .session_boundary(request)
        .await
    }

    #[doc(hidden)]
    pub async fn lcm_session_providers_for_test(
        &self,
        session_id: &str,
    ) -> std::result::Result<Vec<String>, tracedecay_sessions::runtime::lcm::LcmError> {
        self.project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref())
            .lcm_session_providers(session_id)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_session_replay_slice_for_test(
        &self,
        request: &tracedecay_sessions::runtime::lcm::LcmSessionReplayRequest,
    ) -> std::result::Result<
        tracedecay_sessions::runtime::lcm::LcmSessionReplaySlice,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref())
            .lcm_session_replay_slice(request)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_status_deep_for_test(
        &self,
        provider: &str,
        session_id: Option<&str>,
    ) -> std::result::Result<
        tracedecay_sessions::runtime::lcm::LcmStatus,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref())
            .lcm_status_with_options(
                provider,
                session_id,
                true,
                &tracedecay_sessions::runtime::lcm::LcmGcConfig::default(),
            )
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_status_for_test(
        &self,
        provider: &str,
        session_id: Option<&str>,
    ) -> std::result::Result<
        tracedecay_sessions::runtime::lcm::LcmStatus,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref())
            .lcm_status(provider, session_id)
            .await
    }
}

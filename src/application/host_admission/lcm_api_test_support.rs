use super::*;

impl HostAdmissionTestRuntimeV1 {
    #[doc(hidden)]
    pub async fn lcm_compress_for_test(
        &self,
        request: crate::sessions::lcm::LcmCompressionRequest,
    ) -> std::result::Result<
        crate::sessions::lcm::LcmCompressionResponse,
        crate::sessions::lcm::LcmError,
    > {
        self.project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref())
            .lcm_compress(request)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_describe_for_test(
        &self,
        request: crate::sessions::lcm::LcmDescribeRequest,
    ) -> std::result::Result<
        crate::sessions::lcm::LcmDescribeResponse,
        crate::sessions::lcm::LcmError,
    > {
        self.project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref())
            .lcm_describe(request)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_doctor_for_test(
        &self,
        provider: &str,
        session_id: Option<&str>,
        mode: &str,
        apply: bool,
        clean_config: crate::sessions::lcm::LcmCleanConfig,
        gc_config: crate::sessions::lcm::LcmGcConfig,
    ) -> std::result::Result<serde_json::Value, crate::sessions::lcm::LcmError> {
        self.project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref())
            .lcm_doctor(provider, session_id, mode, apply, clean_config, gc_config)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_expand_for_test(
        &self,
        request: crate::sessions::lcm::LcmExpandRequest,
    ) -> std::result::Result<crate::sessions::lcm::LcmExpandResponse, crate::sessions::lcm::LcmError>
    {
        self.project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref())
            .lcm_expand(request)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_expand_query_for_test(
        &self,
        request: crate::sessions::lcm::LcmExpandQueryRequest,
    ) -> std::result::Result<
        crate::sessions::lcm::LcmExpandQueryResponse,
        crate::sessions::lcm::LcmError,
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
        crate::sessions::lcm::LcmSummaryExpansion,
        crate::sessions::lcm::LcmError,
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
        request: crate::sessions::lcm::LcmGrepRequest,
    ) -> std::result::Result<crate::sessions::lcm::LcmGrepOutcome, crate::sessions::lcm::LcmError>
    {
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
        message: &crate::sessions::SessionMessageRecord,
    ) -> std::result::Result<(), crate::sessions::lcm::LcmError> {
        let database = self
            .session_database_for_test(scope)
            .map_err(|error| crate::sessions::lcm::LcmError::Db(error.to_string()))?;
        let storage_root = database.db_path().parent().ok_or_else(|| {
            crate::sessions::lcm::LcmError::Db(
                "registered session database has no storage root".to_string(),
            )
        })?;
        database.lcm_ingest_raw_message(storage_root, message).await
    }

    #[doc(hidden)]
    pub async fn lcm_load_raw_message_for_test(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Option<crate::sessions::lcm::LcmRawMessage> {
        self.project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref())
            .lcm_load_raw_message(provider, message_id)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_load_session_for_test(
        &self,
        request: crate::sessions::lcm::LcmLoadSessionRequest,
    ) -> std::result::Result<crate::sessions::lcm::LcmLoadSessionPage, crate::sessions::lcm::LcmError>
    {
        self.project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref())
            .lcm_load_session(request)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_preflight_for_test(
        &self,
        request: crate::sessions::lcm::LcmPreflightRequest,
    ) -> std::result::Result<
        crate::sessions::lcm::LcmPreflightResponse,
        crate::sessions::lcm::LcmError,
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
        Vec<crate::sessions::lcm::LcmRecentSession>,
        crate::sessions::lcm::LcmError,
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
        config: &crate::sessions::lcm::LcmGcConfig,
        now: i64,
    ) -> std::result::Result<crate::sessions::lcm::LcmGcReport, crate::sessions::lcm::LcmError>
    {
        let database = self
            .session_database_for_test(scope)
            .map_err(|error| crate::sessions::lcm::LcmError::Db(error.to_string()))?;
        let storage_root = database.db_path().parent().ok_or_else(|| {
            crate::sessions::lcm::LcmError::Db(
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
        request: crate::sessions::lcm::LcmSessionBoundaryRequest,
    ) -> std::result::Result<
        crate::sessions::lcm::LcmSessionBoundaryResponse,
        crate::sessions::lcm::LcmError,
    > {
        self.project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref())
            .lcm_session_boundary(request)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_session_providers_for_test(
        &self,
        session_id: &str,
    ) -> std::result::Result<Vec<String>, crate::sessions::lcm::LcmError> {
        self.project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref())
            .lcm_session_providers(session_id)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_session_replay_slice_for_test(
        &self,
        request: &crate::sessions::lcm::LcmSessionReplayRequest,
    ) -> std::result::Result<
        crate::sessions::lcm::LcmSessionReplaySlice,
        crate::sessions::lcm::LcmError,
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
    ) -> std::result::Result<crate::sessions::lcm::LcmStatus, crate::sessions::lcm::LcmError> {
        self.project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref())
            .lcm_status_with_options(
                provider,
                session_id,
                true,
                &crate::sessions::lcm::LcmGcConfig::default(),
            )
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_status_for_test(
        &self,
        provider: &str,
        session_id: Option<&str>,
    ) -> std::result::Result<crate::sessions::lcm::LcmStatus, crate::sessions::lcm::LcmError> {
        self.project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref())
            .lcm_status(provider, session_id)
            .await
    }

    #[doc(hidden)]
    pub async fn pending_codex_compaction_summary_requests_for_test(
        &self,
        session_id: Option<&str>,
        limit: usize,
    ) -> std::result::Result<
        Vec<crate::global_db::PendingCodexCompactionSummary>,
        crate::sessions::lcm::LcmError,
    > {
        self.project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref())
            .pending_codex_compaction_summary_requests(session_id, limit)
            .await
    }

    #[doc(hidden)]
    pub async fn publish_codex_compaction_summary_successor_for_test(
        &self,
        node_id: &str,
        summary_text: &str,
        route: &str,
        model: Option<&str>,
    ) -> std::result::Result<crate::sessions::lcm::LcmSummaryNode, crate::sessions::lcm::LcmError>
    {
        self.project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref())
            .publish_codex_compaction_summary_successor(node_id, summary_text, route, model)
            .await
    }
}

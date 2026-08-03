//! Root composition façade for host automation.

pub use tracedecay_agent_hosts::automation::{
    agent_targets, artifacts, backend, config, fact_proposals, hermes_skill_bridge, host_receipts,
    jobs, lifecycle, managed_skills, memory_curator, memory_digest, outcomes, run_ledger, scheduler,
    session_reflector, skill_frontmatter, skill_materialization, skill_targets, skill_writer,
    staged_notice, text,
};

pub mod runner {
    pub use tracedecay_agent_hosts::automation::runner::*;

    pub use super::run_user_memory_curator_with_backend;
}

pub mod skill_usage {
    pub use tracedecay_agent_hosts::automation::skill_usage::{
        DEFAULT_SKILL_OVERLAP_LIMIT, SKILL_OVERLAP_CONTENT_THRESHOLD,
        SKILL_OVERLAP_TITLE_THRESHOLD, SkillImprovementRecommendation, SkillOverlapCandidate,
        SkillStaleRecommendation, SkillUsageAction, SkillUsageEvent, SkillUsageLedger,
        SkillUsageRecord, SkillUsageSummary, analytics_import_key_for_request,
        list_skill_usage_records, load_skill_usage_ledger, load_skill_usage_record,
        load_skill_usage_records, record_skill_approval, record_skill_usage,
        record_skill_usage_event, save_skill_usage_ledger, skill_improvement_recommendations,
        skill_overlap_candidates, skill_usage_ledger_path, stale_skill_recommendations,
        summarize_skill_usage, summarize_skill_usage_for, sync_skill_usage_metadata,
    };

    fn analytics_event(
        event: &crate::global_db::AnalyticsEventRecord,
    ) -> tracedecay_agent_hosts::ports::AnalyticsEventRecord {
        tracedecay_agent_hosts::ports::AnalyticsEventRecord {
            id: event.id,
            provider: event.provider.clone(),
            project_id: event.project_id.clone(),
            session_id: event.session_id.clone(),
            timestamp: event.timestamp,
            event_kind: event.event_kind.clone(),
            hook_name: event.hook_name.clone(),
            tool_name: event.tool_name.clone(),
            tool_category: event.tool_category.clone(),
            skill_name: event.skill_name.clone(),
            hint_category: event.hint_category.clone(),
            hint_id: event.hint_id.clone(),
            outcome: event.outcome.clone(),
            metadata_json: event.metadata_json.clone(),
        }
    }

    pub async fn ingest_analytics_events(
        profile_root: &std::path::Path,
        events: &[crate::global_db::AnalyticsEventRecord],
    ) -> crate::errors::Result<Vec<SkillUsageRecord>> {
        let events = events.iter().map(analytics_event).collect::<Vec<_>>();
        tracedecay_agent_hosts::automation::skill_usage::ingest_analytics_events(
            profile_root,
            &events,
        )
        .await
    }

    pub async fn ingest_project_analytics_events(
        profile_root: &std::path::Path,
        project_root: &std::path::Path,
        global_db: Option<&crate::global_db::GlobalDb>,
        limit: usize,
    ) -> crate::errors::Result<Vec<SkillUsageRecord>> {
        let Some(global_db) = global_db else {
            return Ok(Vec::new());
        };
        let events = global_db
            .query_analytics_events(&crate::global_db::AnalyticsEventQuery {
                provider: None,
                project_id: Some(crate::global_db::GlobalDb::canonical_project_key(project_root)),
                session_id: None,
                event_kind: None,
                since: None,
                limit,
            })
            .await
            .map_err(|message| crate::errors::TraceDecayError::Config {
                message: format!(
                    "failed to import project analytics into skill usage ledger: {message}"
                ),
            })?;
        ingest_analytics_events(profile_root, &events).await
    }
}

impl tracedecay_agent_hosts::automation::runner::ProjectAutomationStore
    for crate::tracedecay::TraceDecay
{
    fn dashboard_root(&self) -> std::path::PathBuf {
        self.store_layout().dashboard_root.clone()
    }

    fn sessions_db_path(&self) -> std::path::PathBuf {
        self.store_layout().sessions_db_path.clone()
    }

    fn project_root(&self) -> &std::path::Path {
        self.project_root()
    }

    fn open_project_memory_db<'a>(
        &'a self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = crate::errors::Result<crate::db::Database>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(self.open_project_store_db())
    }
}

struct ProjectMemoryCuratorStore<'a>(&'a crate::tracedecay::TraceDecay);

impl tracedecay_agent_hosts::automation::memory_curator::MemoryCuratorStore
    for ProjectMemoryCuratorStore<'_>
{
    fn dashboard_root(&self) -> std::path::PathBuf {
        self.0.store_layout().dashboard_root.clone()
    }

    fn sessions_db_path(&self) -> std::path::PathBuf {
        self.0.store_layout().sessions_db_path.clone()
    }

    fn curate<'a>(
        &'a self,
        request: tracedecay_agent_hosts::automation::memory_curator::MemoryCurationRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = crate::errors::Result<serde_json::Value>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let options = crate::dashboard::memory_curate::MemoryCurateOptions {
                apply: request.apply,
                llm: request.llm,
                llm_ops: request.llm_ops,
                max_clusters: request.max_clusters,
                min_confidence: request.min_confidence,
            };
            crate::dashboard::run_memory_curate(self.0, &options).await
        })
    }

    fn refresh_digest<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            if let Ok(project_db) = self.0.open_project_store_db().await {
                memory_digest::refresh_memory_digest_after_memory_change(
                    project_db.conn(),
                    &self.0.store_layout().project_root,
                )
                .await;
            }
        })
    }
}

impl tracedecay_agent_hosts::automation::memory_curator::MemoryCuratorStore
    for crate::tracedecay::TraceDecay
{
    fn dashboard_root(&self) -> std::path::PathBuf {
        ProjectMemoryCuratorStore(self).dashboard_root()
    }

    fn sessions_db_path(&self) -> std::path::PathBuf {
        ProjectMemoryCuratorStore(self).sessions_db_path()
    }

    fn curate<'a>(
        &'a self,
        request: tracedecay_agent_hosts::automation::memory_curator::MemoryCurationRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = crate::errors::Result<serde_json::Value>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let options = crate::dashboard::memory_curate::MemoryCurateOptions {
                apply: request.apply,
                llm: request.llm,
                llm_ops: request.llm_ops,
                max_clusters: request.max_clusters,
                min_confidence: request.min_confidence,
            };
            crate::dashboard::run_memory_curate(self, &options).await
        })
    }

    fn refresh_digest<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            if let Ok(project_db) = self.open_project_store_db().await {
                memory_digest::refresh_memory_digest_after_memory_change(
                    project_db.conn(),
                    &self.store_layout().project_root,
                )
                .await;
            }
        })
    }
}

struct UserMemoryCuratorStore<'a> {
    profile_root: &'a std::path::Path,
    db: &'a crate::db::Database,
}

impl tracedecay_agent_hosts::automation::memory_curator::MemoryCuratorStore
    for UserMemoryCuratorStore<'_>
{
    fn dashboard_root(&self) -> std::path::PathBuf {
        runner::user_automation_root(self.profile_root)
    }

    fn sessions_db_path(&self) -> std::path::PathBuf {
        crate::sessions::user_sessions_db_path(self.profile_root)
    }

    fn curate<'a>(
        &'a self,
        request: tracedecay_agent_hosts::automation::memory_curator::MemoryCurationRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = crate::errors::Result<serde_json::Value>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let memory_db_path = crate::memory::user::user_memory_db_path(self.profile_root);
            let dashboard_root = runner::user_automation_root(self.profile_root);
            let options = crate::dashboard::memory_curate::MemoryCurateOptions {
                apply: request.apply,
                llm: request.llm,
                llm_ops: request.llm_ops,
                max_clusters: request.max_clusters,
                min_confidence: request.min_confidence,
            };
            crate::dashboard::memory_curate::run_user_memory_curate(
                self.db,
                &memory_db_path,
                self.profile_root,
                &dashboard_root,
                &options,
            )
            .await
        })
    }

    fn refresh_digest<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(std::future::ready(()))
    }
}

pub async fn run_user_memory_curator_with_backend(
    profile_root: &std::path::Path,
    config: &config::AutomationConfig,
    backend: &dyn backend::AgentTaskBackend,
    options: memory_curator::MemoryCuratorAutomationOptions,
) -> crate::errors::Result<memory_curator::MemoryCuratorAutomationRun> {
    let db = crate::memory::user::open_user_memory_db(profile_root).await?;
    let store = UserMemoryCuratorStore {
        profile_root,
        db: &db,
    };
    runner::run_memory_curator_with_backend(&store, config, backend, options).await
}
